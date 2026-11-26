use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::rc::Rc;
use std::time::{Duration, Instant};

use anyhow::Result;
use collections::{HashMap, HashSet};
use editor::{Editor, EditorEvent, MultiBufferOffset};
use gpui::{
    AnyElement, App, Context, ElementId, Entity, FontWeight, Hsla, Pixels, Rgba, SharedString,
    Subscription, Task, TaskExt as _, Window, div, px,
};
use i18n::t;
use serde_json::{Value, json};
use theme::ActiveTheme as _;
use ui::{Tooltip, prelude::*};
use util::ResultExt as _;

use super::{ChatView, RowKind, Turn};
use crate::AgentKind;
use crate::chat_style::{flavour_word, text_faint, ui};
use crate::session::{AgentSession, Entry, ToolEntry, ToolStatus};
use agent_harness::ToolCall;

/// Temporary, user-approved stand-in: the CLI only accepts the "desktop", "mobile" and "vscode" surfaces.
pub(crate) const SURFACE: &str = "vscode";
const CAPABILITY: &str = "ui_surface_v1";
const MAX_DEPTH: usize = 64;
const MAX_NODES: usize = 5_000;
const MAX_TOASTS: usize = 5;
const MAX_DRAWN_BY: usize = 20;
/// What the CLI sent for a toast without its own timeout in the recording.
const DEFAULT_TOAST_TIMEOUT: Duration = Duration::from_millis(4000);
/// The extension's instance ids for the spinner and the band above the prompt.
const SPINNER: &str = "main";
const ABOVE_PROMPT: &str = "above-prompt";
/// The extension cuts the tool output it shows plugins at 64 Ki characters.
const MAX_OUTPUT_CHARS: usize = 65_536;
const ABOVE_PROMPT_ROWS: u32 = 12;
/// The extension asks again for a hooked agent row this often, since its drawing can show time passing.
const AGENT_ROW_REFRESH: Duration = Duration::from_secs(5);
const MAX_PANES: usize = 16;

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct TextStyleHint {
    pub dim: bool,
    pub bold: bool,
}

/// What `ui_press`, `ui_input` and `ui_select` send back for a node, exactly as the node carried it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Target {
    pub plugin: String,
    pub handle: serde_json::Number,
    pub key: Option<String>,
}

/// What a click or an edit in a plugin's drawing asks for; the first three are sent as `ui_press`, `ui_input` and `ui_select`.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum UiAction {
    Press {
        target: Target,
        href: Option<String>,
    },
    Input {
        target: Target,
        value: String,
        kind: Option<String>,
    },
    Select {
        target: Target,
        value: String,
    },
    /// A `Link` node: it has no `press`, so wu opens it itself.
    Open {
        href: String,
    },
}

type ActionHandler = Rc<dyn Fn(UiAction, &mut Window, &mut App)>;
/// A drawn component instance: its component name and instance id.
type Site = (String, String);

/// Only web links open: a plugin must not make wu open a file, run a script or follow one of wu's own links.
pub(crate) fn opens_href(href: &str) -> bool {
    !href
        .chars()
        .any(|character| character.is_whitespace() || character.is_control())
        && href.split_once("://").is_some_and(|(scheme, _)| {
            scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https")
        })
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SelectOption {
    pub value: String,
    pub label: SharedString,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum UiNode {
    Text {
        text: SharedString,
        style: TextStyleHint,
    },
    Group {
        children: Vec<UiNode>,
        horizontal: bool,
    },
    Button {
        label: SharedString,
        target: Target,
    },
    /// Opened by the host itself; it has no `press`.
    Link {
        label: SharedString,
        href: String,
    },
    Input {
        label: Option<SharedString>,
        placeholder: Option<SharedString>,
        value: String,
        target: Target,
    },
    Select {
        label: Option<SharedString>,
        value: Option<String>,
        options: Vec<SelectOption>,
        target: Target,
    },
    /// Where the host draws its own component: `reference` 0 with the original props, any other with the props of the answer.
    Engine {
        reference: u64,
    },
    Unknown {
        kind: String,
        text: SharedString,
    },
}

/// Never fails: anything it can't read becomes `Unknown` with its text, nesting stops at `MAX_DEPTH` and reading stops after `MAX_NODES` nodes.
pub(crate) fn parse_tree(tree: &Value) -> UiNode {
    let mut budget = MAX_NODES;
    parse_node(tree, 1, &mut budget)
}

fn parse_node(node: &Value, depth: usize, budget: &mut usize) -> UiNode {
    *budget = budget.saturating_sub(1);
    if let Value::String(text) = node {
        return UiNode::Text {
            text: text.clone().into(),
            style: TextStyleHint::default(),
        };
    }
    let kind = node.get("type").and_then(Value::as_str).unwrap_or_default();
    known_node(kind, node, depth, budget).unwrap_or_else(|| UiNode::Unknown {
        kind: kind.to_owned(),
        text: text_content(node, budget),
    })
}

fn known_node(kind: &str, node: &Value, depth: usize, budget: &mut usize) -> Option<UiNode> {
    let label = |name: &str| prop_text(node, name).map(SharedString::from);
    Some(match kind {
        // Recursion is bounded by the depth check; a deeper box falls back to its text, read without recursion.
        // The CLI schema allows `div`, `span` and `b` as aliases of `Box` and `Text`.
        "Box" | "div" if depth < MAX_DEPTH => {
            let mut children = Vec::new();
            for child in node_children(node) {
                if *budget == 0 {
                    break;
                }
                children.push(parse_node(child, depth + 1, budget));
            }
            UiNode::Group {
                children,
                horizontal: !prop_text(node, "flexDirection")
                    .is_some_and(|direction| direction.starts_with("column")),
            }
        }
        "Text" | "span" | "b" => UiNode::Text {
            text: text_content(node, budget),
            style: TextStyleHint {
                dim: prop_flag(node, "dimColor"),
                bold: kind == "b" || prop_flag(node, "bold"),
            },
        },
        "Button" => UiNode::Button {
            target: target(node)?,
            label: label("label").unwrap_or_else(|| text_content(node, budget)),
        },
        "Link" => {
            let href = prop_text(node, "href")?.to_owned();
            let text = text_content(node, budget);
            UiNode::Link {
                label: if text.is_empty() {
                    label("label").unwrap_or_else(|| href.clone().into())
                } else {
                    text
                },
                href,
            }
        }
        "Input" => UiNode::Input {
            target: target(node)?,
            label: label("label"),
            placeholder: label("placeholder"),
            value: prop_text(node, "value").unwrap_or_default().to_owned(),
        },
        "Select" => {
            let target = target(node)?;
            let mut options = Vec::new();
            for option in node
                .pointer("/props/options")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                if *budget == 0 {
                    break;
                }
                *budget -= 1;
                let Some(value) = option.get("value").and_then(Value::as_str) else {
                    continue;
                };
                let label = option.get("label").and_then(Value::as_str).unwrap_or(value);
                options.push(SelectOption {
                    value: value.to_owned(),
                    label: label.to_owned().into(),
                });
            }
            UiNode::Select {
                target,
                label: label("label"),
                value: prop_text(node, "value").map(str::to_owned),
                options,
            }
        }
        "engine" => UiNode::Engine {
            reference: node.get("ref").and_then(Value::as_u64)?,
        },
        _ => return None,
    })
}

fn node_children(node: &Value) -> &[Value] {
    node.get("children")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
}

fn prop_text<'a>(node: &'a Value, name: &str) -> Option<&'a str> {
    node.get("props")?.get(name)?.as_str()
}

fn prop_flag(node: &Value, name: &str) -> bool {
    node.get("props")
        .and_then(|props| props.get(name))
        .and_then(Value::as_bool)
        .unwrap_or_default()
}

fn target(node: &Value) -> Option<Target> {
    let press = node.get("press")?;
    Some(Target {
        plugin: press.get("plugin")?.as_str()?.to_owned(),
        handle: press.get("handle")?.as_number()?.clone(),
        key: prop_text(node, "key").map(str::to_owned),
    })
}

/// The text a node shows, read depth-first with an explicit stack so no nesting can overflow the call stack.
fn text_content(node: &Value, budget: &mut usize) -> SharedString {
    let mut text = String::new();
    let mut stack = vec![node];
    while let Some(node) = stack.pop() {
        match node {
            Value::String(piece) => text.push_str(piece),
            Value::Object(_) => {
                if let Some(piece) = prop_text(node, "text").or_else(|| prop_text(node, "label")) {
                    text.push_str(piece);
                }
                stack.extend(node_children(node).iter().rev());
            }
            _ => {}
        }
        if *budget == 0 {
            break;
        }
        *budget -= 1;
    }
    text.into()
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PaneInfo {
    pub id: String,
    pub plugin: String,
    pub title: String,
    pub close_on_escape: bool,
    /// Only a color that parses as hex; anything else leaves wu's own background.
    pub background: Option<Hsla>,
    pub rows: Option<u32>,
    pub columns: Option<u32>,
}

impl PaneInfo {
    /// Like the extension, an entry without an `id` or a `plugin` is skipped and a missing title shows the id.
    fn from_roster(pane: &Value) -> Option<Self> {
        let id = pane["id"].as_str()?.to_owned();
        let dimension = |name: &str| {
            pane[name]
                .as_u64()
                .and_then(|value| u32::try_from(value).ok())
        };
        Some(Self {
            plugin: pane["plugin"].as_str()?.to_owned(),
            title: pane["title"].as_str().unwrap_or(&id).to_owned(),
            close_on_escape: pane["close_on_escape"].as_bool().unwrap_or_default(),
            background: pane["background_color"]
                .as_str()
                .and_then(|color| Rgba::try_from(color).ok())
                .map(Hsla::from),
            rows: dimension("rows"),
            columns: dimension("columns"),
            id,
        })
    }
}

/// The pane's size in cells: its preferred `columns` and `rows`, never more than the side pane fits.
pub(crate) fn pane_cells(pane: &PaneInfo, fit_columns: u32, fit_rows: u32) -> (u32, u32) {
    let clamp = |preferred: Option<u32>, fit: u32| preferred.unwrap_or(fit).min(fit).max(1);
    (clamp(pane.columns, fit_columns), clamp(pane.rows, fit_rows))
}

/// How many rows a drawing takes, counting each leaf as one row.
fn rows_of(node: &UiNode) -> u32 {
    match node {
        UiNode::Group {
            children,
            horizontal: false,
        } => children.iter().map(rows_of).sum(),
        UiNode::Group { children, .. } => children.iter().map(rows_of).max().unwrap_or(0),
        _ => 1,
    }
}

pub(crate) struct Toast {
    pub plugin: String,
    pub text: SharedString,
    pub expires_at: Instant,
}

pub(crate) struct Rendered {
    pub tree: UiNode,
    pub drawn_by: Vec<String>,
    pub hooked: bool,
}

impl Rendered {
    fn from_answer(answer: &Value) -> Self {
        Self {
            tree: parse_tree(&answer["tree"]),
            drawn_by: answer["drawn_by"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .take(MAX_DRAWN_BY)
                .map(str::to_owned)
                .collect(),
            hooked: answer["hooked"].as_bool().unwrap_or(false),
        }
    }

    /// Claude Code's own drawing, an unhooked component or a tree that only holds wu's row leaves the row as wu draws it.
    fn shows(&self) -> bool {
        self.hooked && !self.drawn_by.is_empty() && draws_anything(&self.tree)
    }
}

fn draws_anything(node: &UiNode) -> bool {
    match node {
        UiNode::Text { text, .. } | UiNode::Unknown { text, .. } => !text.trim().is_empty(),
        UiNode::Group { children, .. } => children.iter().any(draws_anything),
        UiNode::Engine { .. } => false,
        UiNode::Button { .. }
        | UiNode::Link { .. }
        | UiNode::Input { .. }
        | UiNode::Select { .. } => true,
    }
}

fn collect_controls(site: &Site, node: &UiNode, keys: &mut HashSet<String>) {
    match node {
        UiNode::Group { children, .. } => {
            for child in children {
                collect_controls(site, child, keys);
            }
        }
        UiNode::Input { target, .. } | UiNode::Select { target, .. } => {
            keys.insert(control_key(site, target));
        }
        _ => {}
    }
}

/// Names a control across renders: its plugin hands out a new `handle` for every drawing, so the `key` is used when it has one.
fn control_key(site: &Site, target: &Target) -> String {
    let name = target
        .key
        .clone()
        .unwrap_or_else(|| target.handle.to_string());
    format!("{}:{}:{}:{name}", site.0, site.1, target.plugin)
}

struct PluginInput {
    editor: Entity<Editor>,
    /// Updated with every drawing, since the handle changes.
    target: Target,
    /// Text wu put in the field itself, so the edit it causes isn't sent back as typing.
    applied: Option<String>,
    _subscription: Subscription,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Attachment {
    Detached,
    /// Sent with the number of that attach, so an answer to an earlier one is ignored.
    Pending(u64),
    Attached,
    /// Not asked again until the chat is shown again, so a refusing agent isn't asked every frame.
    Refused,
}

/// One chat view's attachment to its agent's plugin drawing, and what the plugins drew so far in the current run.
pub(crate) struct PluginUi {
    client_id: String,
    /// The run this state belongs to; each run is a new agent process that knows nothing of the last one.
    run: u64,
    attachment: Attachment,
    attaches_sent: u64,
    pub statuses: BTreeMap<String, SharedString>,
    pub toasts: VecDeque<Toast>,
    pub panes: Vec<PaneInfo>,
    pub shown_pane: Option<String>,
    pub focused_pane: Option<String>,
    /// Taken by the first frame that sees it, so focus moves once.
    pub focus_requested: Option<String>,
    /// A pane appeared or another one was shown: the next frame opens the side pane for it.
    pub reveal_panes: bool,
    pub pane_offsets: HashMap<String, u32>,
    pub rem: Pixels,
    cache: HashMap<Site, Rendered>,
    /// Invalidated drawings: still shown, so the row keeps its height, until the new one arrives.
    stale: HashSet<Site>,
    /// Renders asked for and not cached, with the number of the latest request; a failed one stays here so it isn't asked again every frame.
    requested: HashMap<Site, u64>,
    requests_sent: u64,
    /// The props of each instance's latest request and when it was sent: a drawing is asked for again when wu's props for it change.
    asked: HashMap<Site, (Value, Instant)>,
    /// Components the agent answered `hooked: false` for: not asked again until the next `ui_invalidate`.
    unhooked: HashSet<String>,
    /// Drawings the last frame looked for and didn't have, sent by `ChatView::flush_plugin_ui` once the frame is built.
    wanted: RefCell<Vec<(Site, Value)>>,
    inputs: HashMap<String, PluginInput>,
    /// The option last picked in each `Select`, shown until the plugin draws the control again.
    selections: HashMap<String, String>,
    columns: u32,
    toast_timer: Option<Task<()>>,
}

impl PluginUi {
    pub fn new() -> Self {
        Self {
            client_id: uuid::Uuid::new_v4().to_string(),
            run: 0,
            attachment: Attachment::Detached,
            attaches_sent: 0,
            statuses: BTreeMap::new(),
            toasts: VecDeque::new(),
            panes: Vec::new(),
            shown_pane: None,
            focused_pane: None,
            focus_requested: None,
            reveal_panes: false,
            pane_offsets: HashMap::default(),
            rem: px(16.),
            cache: HashMap::default(),
            stale: HashSet::default(),
            requested: HashMap::default(),
            requests_sent: 0,
            asked: HashMap::default(),
            unhooked: HashSet::default(),
            wanted: RefCell::default(),
            inputs: HashMap::default(),
            selections: HashMap::default(),
            columns: 0,
            toast_timer: None,
        }
    }

    pub fn available(capabilities: &[String]) -> bool {
        capabilities
            .iter()
            .any(|capability| capability == CAPABILITY)
    }

    /// Forgets what an earlier run drew once `run` is a different one; the request counters carry on so a late answer from the old run can't match a new request.
    pub fn follow_run(&mut self, run: u64) {
        if self.run != run {
            *self = Self {
                client_id: std::mem::take(&mut self.client_id),
                run,
                attaches_sent: self.attaches_sent,
                requests_sent: self.requests_sent,
                ..Self::new()
            };
        }
    }

    fn wants_attach(&self, session: &AgentSession) -> bool {
        self.attachment == Attachment::Detached
            && session.has_live_run()
            && session.kind() == AgentKind::Claude
            && Self::available(session.capabilities())
    }

    pub fn attach(
        &mut self,
        session: &Entity<AgentSession>,
        columns: u32,
        rows: u32,
        cx: &mut Context<ChatView>,
    ) -> Task<Result<()>> {
        self.attaches_sent += 1;
        let number = self.attaches_sent;
        self.attachment = Attachment::Pending(number);
        self.columns = columns;
        let request = json!({
            "subtype": "ui_attach",
            "surface": SURFACE,
            "client_id": self.client_id,
            "viewport": { "columns": columns.max(1), "rows": rows.max(1), "isFullscreen": false },
            "answers": ["ui_copy", "ui_prompt_read", "ui_prompt_fill", "ui_prompt_suggest"],
        });
        let answer = session.update(cx, |session, cx| session.control(request, cx));
        cx.spawn(async move |this, cx| {
            let answer = answer.await;
            this.update(cx, |view, cx| {
                let plugin_ui = &mut view.plugin_ui;
                if plugin_ui.attachment == Attachment::Pending(number) {
                    plugin_ui.attachment = if answer.is_ok() {
                        Attachment::Attached
                    } else {
                        Attachment::Refused
                    };
                    cx.notify();
                }
            })?;
            answer.map(|_| ())
        })
    }

    pub fn detach(&mut self, session: &Entity<AgentSession>, cx: &mut App) {
        self.follow_run(session.read(cx).runs_started());
        self.panes.clear();
        self.shown_pane = None;
        self.focused_pane = None;
        self.focus_requested = None;
        self.pane_offsets.clear();
        let attachment = std::mem::replace(&mut self.attachment, Attachment::Detached);
        // A pending attach is detached too: the agent handles both in the order they were sent.
        if !matches!(attachment, Attachment::Attached | Attachment::Pending(_)) {
            return;
        }
        self.cache.clear();
        self.stale.clear();
        self.requested.clear();
        self.asked.clear();
        self.inputs.clear();
        self.selections.clear();
        // Without a live run a control request would start a one-off agent process just to detach.
        if !session.read(cx).has_live_run() {
            return;
        }
        let request = json!({ "subtype": "ui_detach", "client_id": self.client_id });
        session
            .update(cx, |session, cx| session.control(request, cx))
            .detach_and_log_err(cx);
    }

    /// The cached drawing of a component instance; when there is none yet, it is stale or `props` changed, it is asked for in the background and the view notified once it arrives.
    pub fn render(
        &mut self,
        session: &Entity<AgentSession>,
        component: &str,
        instance_id: &str,
        props: Value,
        window: &mut Window,
        cx: &mut Context<ChatView>,
    ) -> Option<&Rendered> {
        let key = (component.to_owned(), instance_id.to_owned());
        if !self.may_ask(&key) || !self.outdated(&key, &props) {
            return self.cache.get(&key);
        }
        self.requests_sent += 1;
        let number = self.requests_sent;
        self.requested.insert(key.clone(), number);
        self.asked
            .insert(key.clone(), (props.clone(), Instant::now()));
        let viewport = (component == "Pane").then(|| {
            json!({ "columns": props["bodyColumns"], "rows": props["scroll"]["bodyRows"], "isFullscreen": false })
        });
        let mut request = json!({
            "subtype": "ui_render",
            "surface": SURFACE,
            "client_id": self.client_id,
            "component": component,
            "instance_id": instance_id,
            "props": props,
        });
        if let Some(viewport) = viewport {
            request["viewport"] = viewport;
        }
        let answer = session.update(cx, |session, cx| session.control(request, cx));
        let answered_key = key.clone();
        cx.spawn_in(window, async move |this, cx| {
            let key = answered_key;
            let answer = answer.await;
            this.update_in(cx, |view, window, cx| {
                // An invalidation or a newer request made this answer stale.
                if view.plugin_ui.requested.get(&key) != Some(&number) {
                    return;
                }
                match answer {
                    Ok(answer) => {
                        view.plugin_ui.requested.remove(&key);
                        view.plugin_ui.stale.remove(&key);
                        if answer["hooked"] == false {
                            view.plugin_ui.unhooked.insert(key.0.clone());
                        }
                        let rendered = Rendered::from_answer(&answer);
                        view.sync_plugin_inputs(&key, &rendered.tree, window, cx);
                        view.plugin_ui.cache.insert(key, rendered);
                        view.plugin_ui.prune_controls();
                        cx.notify();
                    }
                    Err(error) => log::warn!("the agent didn't draw {}: {error:#}", key.0),
                }
            })
            .log_err();
        })
        .detach();
        self.cache.get(&key)
    }

    /// The drawing to show in place of a component instance, if a plugin drew one; a missing, stale or outdated drawing is asked for by the next `ChatView::flush_plugin_ui`, and the old one stays on screen meanwhile.
    fn drawn(
        &self,
        component: &str,
        instance_id: &str,
        props: impl FnOnce() -> Value,
    ) -> Option<&Rendered> {
        let key = (component.to_owned(), instance_id.to_owned());
        // A request in flight isn't doubled: once it is answered, the next frame asks again if the props moved on meanwhile.
        if self.may_ask(&key) {
            let props = props();
            if self.outdated(&key, &props) {
                self.wanted.borrow_mut().push((key.clone(), props));
            }
        }
        self.cache.get(&key).filter(|rendered| rendered.shows())
    }

    fn may_ask(&self, key: &Site) -> bool {
        self.attachment == Attachment::Attached
            && !self.requested.contains_key(key)
            && !self.unhooked.contains(&key.0)
    }

    fn outdated(&self, key: &Site, props: &Value) -> bool {
        let (Some(cached), Some((asked, asked_at))) = (self.cache.get(key), self.asked.get(key))
        else {
            return true;
        };
        self.stale.contains(key)
            || asked != props
            || (key.0 == "AgentRow" && cached.hooked && asked_at.elapsed() >= AGENT_ROW_REFRESH)
    }

    /// Drops the fields and picks of controls no current drawing has, such as a keyless `Input` whose handle changed.
    fn prune_controls(&mut self) {
        let mut live = HashSet::default();
        for (site, rendered) in &self.cache {
            collect_controls(site, &rendered.tree, &mut live);
        }
        self.inputs.retain(|key, _| live.contains(key));
        self.selections.retain(|key, _| live.contains(key));
    }

    /// Keeps the drawing on screen until the new one arrives, so the row doesn't change height in between.
    fn invalidate(&mut self, key: &Site) {
        self.requested.remove(key);
        if self.cache.contains_key(key) {
            self.stale.insert(key.clone());
        }
    }

    /// Applies a drawing frame from the agent; true when the view must draw again.
    pub fn apply_event(&mut self, event: &Value, now: Instant) -> bool {
        let text = |event: &Value| event["text"].as_str().map(|text| text.to_owned().into());
        let plugin = event["plugin"].as_str().unwrap_or_default().to_owned();
        match event["subtype"].as_str().unwrap_or_default() {
            "ui_toast" => {
                let Some(text) = text(event) else {
                    return false;
                };
                let timeout = event["timeout_ms"]
                    .as_u64()
                    .map_or(DEFAULT_TOAST_TIMEOUT, Duration::from_millis);
                self.toasts.push_back(Toast {
                    plugin,
                    text,
                    expires_at: now + timeout,
                });
                while self.toasts.len() > MAX_TOASTS {
                    self.toasts.pop_front();
                }
                true
            }
            "ui_status" => {
                match text(event) {
                    Some(text) => self.statuses.insert(plugin, text),
                    None => self.statuses.remove(&plugin),
                };
                true
            }
            "ui_log" => {
                log::info!("{plugin}: {}", event["text"].as_str().unwrap_or_default());
                false
            }
            "ui_invalidate" => {
                self.unhooked.clear();
                match event["instances"].as_array() {
                    Some(instances) => {
                        for instance in instances {
                            let key = (
                                instance["component"]
                                    .as_str()
                                    .unwrap_or_default()
                                    .to_owned(),
                                instance["instance_id"]
                                    .as_str()
                                    .unwrap_or_default()
                                    .to_owned(),
                            );
                            self.invalidate(&key);
                        }
                    }
                    None => {
                        self.requested.clear();
                        self.stale = self.cache.keys().cloned().collect();
                    }
                }
                true
            }
            "ui_panes" => {
                let panes = event["panes"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(PaneInfo::from_roster)
                    .take(MAX_PANES)
                    .collect::<Vec<_>>();
                let shown = event["shown_id"].as_str().map(str::to_owned);
                // The CLI sends the roster again after every attach; only a change opens the side pane.
                self.reveal_panes |= panes
                    .iter()
                    .any(|pane| self.panes.iter().all(|old| old.id != pane.id))
                    || (shown.is_some() && shown != self.shown_pane);
                self.panes = panes;
                self.shown_pane = shown;
                self.focused_pane = event["focused_id"].as_str().map(str::to_owned);
                self.focus_requested = event["focus_requested_id"].as_str().map(str::to_owned);
                true
            }
            _ => false,
        }
    }

    /// The pane `shown_id` names, or the first one when it names none of them.
    pub fn visible_pane(&self) -> Option<&PaneInfo> {
        self.panes
            .iter()
            .find(|pane| self.shown_pane.as_ref() == Some(&pane.id))
            .or(self.panes.first())
    }

    pub fn pane_content_rows(&self, id: &str) -> u32 {
        self.cache
            .get(&("Pane".to_owned(), id.to_owned()))
            .map_or(0, |rendered| rows_of(&rendered.tree))
    }

    /// Sends a pane request and applies its answer, unless the chat detached or started another run meanwhile.
    fn pane_request(
        &self,
        session: &Entity<AgentSession>,
        request: Value,
        cx: &mut Context<ChatView>,
        apply: impl FnOnce(&mut PluginUi, &Value) + 'static,
    ) {
        if self.attachment != Attachment::Attached {
            return;
        }
        let run = self.run;
        let answer = session.update(cx, |session, cx| session.control(request, cx));
        cx.spawn(async move |this, cx| {
            let answer = answer.await?;
            this.update(cx, |view, cx| {
                if view.plugin_ui.run == run && view.plugin_ui.attachment == Attachment::Attached {
                    apply(&mut view.plugin_ui, &answer);
                    cx.notify();
                }
            })
        })
        .detach_and_log_err(cx);
    }

    pub fn show_pane(&self, session: &Entity<AgentSession>, id: &str, cx: &mut Context<ChatView>) {
        let request = json!({ "subtype": "ui_pane_show", "id": id, "surface": SURFACE, "client_id": self.client_id });
        self.pane_request(session, request, cx, |plugin_ui, answer| {
            plugin_ui.shown_pane = answer["shown_id"].as_str().map(str::to_owned);
        });
    }

    /// `None` tells the agent that no pane has focus.
    pub fn focus_pane(
        &self,
        session: &Entity<AgentSession>,
        id: Option<&str>,
        cx: &mut Context<ChatView>,
    ) {
        let request = json!({ "subtype": "ui_pane_focus", "id": id, "surface": SURFACE, "client_id": self.client_id });
        self.pane_request(session, request, cx, |plugin_ui, answer| {
            plugin_ui.focused_pane = answer["focused_id"].as_str().map(str::to_owned);
        });
    }

    /// The pane stays until its plugin agrees to close it.
    pub fn close_pane(&self, session: &Entity<AgentSession>, id: &str, cx: &mut Context<ChatView>) {
        let request = json!({ "subtype": "ui_close", "id": id, "client_id": self.client_id });
        let id = id.to_owned();
        self.pane_request(session, request, cx, move |plugin_ui, answer| {
            if answer["closed"] != true {
                return;
            }
            plugin_ui.panes.retain(|pane| pane.id != id);
            if plugin_ui.shown_pane.as_ref() == Some(&id) {
                plugin_ui.shown_pane = None;
            }
            plugin_ui.pane_offsets.remove(&id);
            plugin_ui.cache.remove(&("Pane".to_owned(), id));
            plugin_ui.prune_controls();
        });
    }

    pub fn scroll_pane(
        &self,
        session: &Entity<AgentSession>,
        id: &str,
        by: i32,
        body_rows: u32,
        content_rows: u32,
        cx: &mut Context<ChatView>,
    ) {
        let request = json!({
            "subtype": "ui_scroll",
            "surface": SURFACE,
            "client_id": self.client_id,
            "component": "Pane",
            "instance_id": id,
            "offset": self.pane_offsets.get(id).copied().unwrap_or(0),
            "by": by,
            "body_rows": body_rows,
            "content_rows": content_rows,
        });
        let id = id.to_owned();
        self.pane_request(session, request, cx, move |plugin_ui, answer| {
            // A refused scroll leaves the pane where it was.
            if answer["deny"].is_string() {
                return;
            }
            if let Some(offset) = answer["offset"]
                .as_u64()
                .and_then(|offset| u32::try_from(offset).ok())
            {
                plugin_ui.pane_offsets.insert(id, offset);
            }
        });
    }

    /// Drops the toasts whose time is up at `now`; true when any went.
    pub fn expire_toasts(&mut self, now: Instant) -> bool {
        let before = self.toasts.len();
        self.toasts.retain(|toast| toast.expires_at > now);
        self.toasts.len() != before
    }

    #[cfg(test)]
    pub fn client_id(&self) -> &str {
        &self.client_id
    }

    #[cfg(test)]
    pub fn input_texts(&self, cx: &App) -> Vec<String> {
        self.inputs
            .values()
            .map(|input| input.editor.read(cx).text(cx))
            .collect()
    }
}

impl ChatView {
    /// Attaches from draw time, since only a frame being drawn proves the chat is visible; `Item::deactivated` detaches.
    pub(super) fn sync_plugin_ui(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.plugin_ui
            .follow_run(self.session.read(cx).runs_started());
        self.plugin_ui.rem = window.rem_size();
        self.sync_plugin_panes(window, cx);
        if !self.plugin_ui.wants_attach(self.session.read(cx)) {
            return;
        }
        let viewport = window.viewport_size();
        let cell = window.rem_size();
        // shortcut: the viewport is counted in cells of 0.6 by 1.4 rem; measure the drawn area once Tasks 48-49 lay trees out.
        let columns = (viewport.width / (cell * 0.6)).floor() as u32;
        let rows = (viewport.height / (cell * 1.4)).floor() as u32;
        self.plugin_ui
            .attach(&self.session, columns, rows, cx)
            .detach_and_log_err(cx);
    }

    pub(super) fn apply_plugin_ui_event(&mut self, event: &Value, cx: &mut Context<Self>) {
        self.plugin_ui
            .follow_run(self.session.read(cx).runs_started());
        let now = cx.background_executor().now();
        if self.plugin_ui.apply_event(event, now) {
            cx.notify();
        }
        if event["subtype"] == "ui_toast" {
            self.schedule_toast_expiry(cx);
        }
    }

    fn schedule_toast_expiry(&mut self, cx: &mut Context<Self>) {
        let Some(next) = self
            .plugin_ui
            .toasts
            .iter()
            .map(|toast| toast.expires_at)
            .min()
        else {
            self.plugin_ui.toast_timer = None;
            return;
        };
        let delay = next.saturating_duration_since(cx.background_executor().now());
        self.plugin_ui.toast_timer = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(delay).await;
            this.update(cx, |view, cx| {
                if view.plugin_ui.expire_toasts(cx.background_executor().now()) {
                    cx.notify();
                }
                view.schedule_toast_expiry(cx);
            })
            .log_err();
        }));
    }

    /// Sends the drawings the frame just built looked for; called once the frame's elements no longer borrow the view.
    pub(super) fn flush_plugin_ui(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for ((component, instance_id), props) in self.plugin_ui.wanted.take() {
            self.plugin_ui
                .render(&self.session, &component, &instance_id, props, window, cx);
        }
    }

    /// A plugin's drawing of a component instance with its caption, or `None` when no plugin drew it; wu's own element `own` goes where the drawing has its `engine` node, and stays in `own` when it has none.
    pub(super) fn plugin_drawing(
        &self,
        component: &str,
        instance_id: &str,
        props: impl FnOnce() -> Value,
        own: &mut Option<AnyElement>,
        cx: &App,
    ) -> Option<AnyElement> {
        let rendered = self.plugin_ui.drawn(component, instance_id, props)?;
        let site = (component.to_owned(), instance_id.to_owned());
        let view = self.this.clone();
        let action_site = site.clone();
        let on_action: ActionHandler = Rc::new(move |action, window, cx| {
            view.update(cx, |view, cx| {
                view.plugin_ui_action(action_site.clone(), action, window, cx)
            })
            .log_err();
        });
        let mut drawing = Drawing {
            plugin_ui: &self.plugin_ui,
            site: &site,
            on_action,
            own: own.take(),
        };
        let tree = drawing.node(&rendered.tree, cx);
        *own = drawing.own.take();
        Some(
            v_flex()
                .w_full()
                .min_w_0()
                .child(tree)
                .child(caption(&rendered.drawn_by, cx))
                .into_any_element(),
        )
    }

    /// `own` as wu draws it, or the plugin drawing that takes its place.
    pub(super) fn plugin_row(
        &self,
        component: &str,
        instance_id: &str,
        props: impl FnOnce() -> Value,
        own: AnyElement,
        cx: &App,
    ) -> AnyElement {
        let mut own = Some(own);
        self.plugin_drawing(component, instance_id, props, &mut own, cx)
            .or(own)
            .unwrap_or_else(|| div().into_any_element())
    }

    /// The transcript rows a plugin can draw, other than tool rows, which their group draws.
    pub(super) fn plugin_transcript_row(
        &self,
        kind: RowKind,
        turn: &Turn,
        own: AnyElement,
        cx: &App,
    ) -> AnyElement {
        let entries = self.entries(cx);
        match kind {
            RowKind::User(index) => {
                let Some(Entry::User { text, .. }) = entries.get(index) else {
                    return own;
                };
                self.plugin_row(
                    "UserMessage",
                    &index.to_string(),
                    || json!({ "text": text, "origin": { "kind": "sdk" }, "isExpanded": true }),
                    own,
                    cx,
                )
            }
            RowKind::Message(index) => {
                let Some(Entry::Assistant { markdown, .. }) = entries.get(index) else {
                    return own;
                };
                self.plugin_row(
                    "AssistantMessage",
                    &index.to_string(),
                    || {
                        json!({
                            "text": markdown.read(cx).source(),
                            "isFirstOfReply": turn.first_reply == Some(index),
                        })
                    },
                    own,
                    cx,
                )
            }
            RowKind::Notice(index) => match entries.get(index) {
                Some(Entry::Notice {
                    text,
                    is_error: false,
                }) => self.plugin_row(
                    "CommandOutput",
                    &index.to_string(),
                    || json!({ "text": text }),
                    own,
                    cx,
                ),
                _ => own,
            },
            RowKind::Working => {
                let session = self.session.read(cx);
                let elapsed = self
                    .working_since(cx)
                    .map_or(0, |since| since.elapsed().as_secs());
                let mode = match entries.last() {
                    Some(Entry::Tool(tool)) if tool.status == ToolStatus::Running => "tool-use",
                    Some(Entry::Assistant { .. } | Entry::Thinking(_)) => "responding",
                    _ => "requesting",
                };
                self.plugin_row(
                    "Spinner",
                    SPINNER,
                    || {
                        json!({
                            "word": flavour_word(self.flavour_seed, elapsed),
                            "message": session.is_compacting().then_some("Compacting"),
                            "suffix": "…",
                            "mode": mode,
                        })
                    },
                    own,
                    cx,
                )
            }
            _ => own,
        }
    }

    pub(super) fn plugin_agent_row(&self, task_id: &str, own: AnyElement, cx: &App) -> AnyElement {
        // The extension leaves 4 columns for the row's own indent.
        let columns = self.plugin_ui.columns.saturating_sub(4);
        self.plugin_row(
            "AgentRow",
            task_id,
            || json!({ "columns": columns }),
            own,
            cx,
        )
    }

    /// What plugins draw above the prompt: the `AbovePrompt` band, the drawing for the question being asked, and toasts.
    pub(super) fn render_plugin_above_prompt(&self, cx: &App) -> Option<AnyElement> {
        let session = self.session.read(cx);
        let band = self.plugin_drawing(
            "AbovePrompt",
            ABOVE_PROMPT,
            || {
                json!({
                    "hasSurvey": false,
                    "isCollapsed": false,
                    "isWorking": session.is_working(),
                    "maxRows": ABOVE_PROMPT_ROWS,
                    "bodyColumns": self.plugin_ui.columns,
                    "scroll": { "offset": 0, "bodyRows": ABOVE_PROMPT_ROWS },
                    "view": {},
                })
            },
            &mut None,
            cx,
        );
        let question = session
            .pending_questions()
            .first()
            .filter(|pending| !pending.is_plan() && !pending.is_permission())
            .and_then(|pending| {
                self.plugin_drawing(
                    "AskUserQuestion",
                    pending.id(),
                    || {
                        let questions = pending
                            .questions
                            .iter()
                            .map(|question| {
                                json!({
                                    "question": question.question,
                                    "header": question.header,
                                    "options": question.options.iter().map(|label| json!({ "label": label })).collect::<Vec<_>>(),
                                    "multiSelect": question.multi_select,
                                })
                            })
                            .collect::<Vec<_>>();
                        json!({ "tool": "AskUserQuestion", "questions": questions })
                    },
                    &mut None,
                    cx,
                )
            });
        if band.is_none() && question.is_none() && self.plugin_ui.toasts.is_empty() {
            return None;
        }
        let colors = cx.theme().colors();
        Some(
            v_flex()
                .w_full()
                .gap(px(4.))
                .pb(px(8.))
                .children(band)
                .children(question)
                .children(self.plugin_ui.toasts.iter().map(|toast| {
                    h_flex()
                        .debug_selector(|| "plugin-ui-toast".into())
                        .gap(px(6.))
                        .px(px(10.))
                        .py(px(4.))
                        .rounded(px(8.))
                        .bg(colors.element_background)
                        .text_size(ui(12.))
                        .child(
                            div()
                                .flex_none()
                                .text_color(text_faint(cx))
                                .child(toast.plugin.clone()),
                        )
                        .child(div().min_w_0().child(toast.text.clone()))
                }))
                .into_any_element(),
        )
    }

    pub(super) fn render_plugin_status(&self, cx: &App) -> Option<AnyElement> {
        if self.plugin_ui.statuses.is_empty() {
            return None;
        }
        let text = self
            .plugin_ui
            .statuses
            .values()
            .map(SharedString::as_ref)
            .collect::<Vec<_>>()
            .join(" · ");
        Some(
            div()
                .debug_selector(|| "plugin-ui-status".into())
                .min_w_0()
                .px(px(8.))
                .truncate()
                .text_size(ui(11.))
                .text_color(text_faint(cx))
                .child(text)
                .into_any_element(),
        )
    }

    pub(super) fn plugin_ui_action(
        &mut self,
        site: Site,
        action: UiAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (subtype, target, fields) = match action {
            UiAction::Open { href } => {
                // Not through `open_link`: a plugin's link must never be resolved to a local file.
                if opens_href(&href) {
                    cx.open_url(&href);
                }
                return;
            }
            UiAction::Press { target, href } => ("ui_press", target, json!({ "href": href })),
            UiAction::Input {
                target,
                value,
                kind,
            } => (
                "ui_input",
                target,
                json!({ "value": value, "kind": kind, "component": site.0, "instance_id": site.1 }),
            ),
            UiAction::Select { target, value } => {
                self.plugin_ui
                    .selections
                    .insert(control_key(&site, &target), value.clone());
                cx.notify();
                (
                    "ui_select",
                    target,
                    json!({ "value": value, "component": site.0, "instance_id": site.1 }),
                )
            }
        };
        let mut request = json!({
            "subtype": subtype,
            "surface": SURFACE,
            "client_id": self.plugin_ui.client_id,
            "plugin": target.plugin,
            "handle": target.handle,
            "key": target.key,
        });
        if let (Some(request), Value::Object(fields)) = (request.as_object_mut(), fields) {
            request.extend(fields);
            request.retain(|_, value| !value.is_null());
        }
        let input = (subtype == "ui_input").then(|| control_key(&site, &target));
        let answer = self
            .session
            .update(cx, |session, cx| session.control(request, cx));
        cx.spawn_in(window, async move |this, cx| {
            let answer = answer.await?;
            this.update_in(cx, |view, window, cx| {
                if let Some(value) = answer["value"].as_str()
                    && let Some(input) = input.and_then(|key| view.plugin_ui.inputs.get_mut(&key))
                    && input.editor.read(cx).text(cx) != value
                {
                    input.applied = Some(value.to_owned());
                    input
                        .editor
                        .update(cx, |editor, cx| editor.set_text(value, window, cx));
                }
                // As the extension does: a control the plugin didn't handle is drawn again, so it can't stay stuck.
                if answer["handled"] != true {
                    view.plugin_ui.invalidate(&site);
                    cx.notify();
                }
            })
        })
        .detach_and_log_err(cx);
    }

    /// Gives each `Input` of a new drawing its text field, kept across drawings by its key.
    fn sync_plugin_inputs(
        &mut self,
        site: &Site,
        node: &UiNode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match node {
            UiNode::Group { children, .. } => {
                for child in children {
                    self.sync_plugin_inputs(site, child, window, cx);
                }
            }
            UiNode::Input {
                placeholder,
                value,
                target,
                ..
            } => {
                let key = control_key(site, target);
                if let Some(input) = self.plugin_ui.inputs.get_mut(&key) {
                    input.target = target.clone();
                    return;
                }
                let editor = cx.new(|cx| {
                    let mut editor = Editor::single_line(window, cx);
                    editor.set_text(value.as_str(), window, cx);
                    if let Some(placeholder) = placeholder {
                        editor.set_placeholder_text(placeholder, window, cx);
                    }
                    editor
                });
                let subscription = cx.subscribe_in(&editor, window, {
                    let key = key.clone();
                    let site = site.clone();
                    move |view, editor, event: &EditorEvent, window, cx| {
                        if !matches!(event, EditorEvent::BufferEdited) {
                            return;
                        }
                        let value = editor.read(cx).text(cx);
                        let Some(input) = view.plugin_ui.inputs.get_mut(&key) else {
                            return;
                        };
                        if input.applied.take().as_ref() == Some(&value) {
                            return;
                        }
                        let target = input.target.clone();
                        view.plugin_ui_action(
                            site.clone(),
                            UiAction::Input {
                                target,
                                value,
                                kind: Some("change".into()),
                            },
                            window,
                            cx,
                        );
                    }
                });
                self.plugin_ui.inputs.insert(
                    key,
                    PluginInput {
                        editor,
                        target: target.clone(),
                        applied: Some(value.clone()),
                        _subscription: subscription,
                    },
                );
            }
            _ => {}
        }
    }

    /// Answers the agent's `ui_prompt_read` and `ui_prompt_fill` from the composer; the cursor is counted in UTF-16 units like the extension's.
    pub(super) fn answer_prompt_requests(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let requests = self
            .session
            .update(cx, |session, _| session.take_prompt_requests());
        for (request, reply) in requests {
            let (text, cursor) = self.composer.update(cx, |editor, cx| {
                let snapshot = editor.display_snapshot(cx);
                let cursor = editor
                    .selections
                    .newest::<MultiBufferOffset>(&snapshot)
                    .head()
                    .0;
                (editor.text(cx), cursor)
            });
            let answer = if request["subtype"] == "ui_prompt_read" {
                let before = text.get(..cursor).unwrap_or(&text);
                json!({ "text": text, "cursor": before.encode_utf16().count() })
            } else {
                let added = request["text"].as_str().unwrap_or_default();
                let filled = match request["mode"].as_str() {
                    Some("replace") => added.to_owned(),
                    Some("append") => format!("{text}{added}"),
                    _ => {
                        let at = if text.is_char_boundary(cursor) {
                            cursor
                        } else {
                            text.len()
                        };
                        format!("{}{added}{}", &text[..at], &text[at..])
                    }
                };
                self.composer.update(cx, |editor, cx| {
                    editor.set_text(filled, window, cx);
                    editor.move_to_end(&editor::actions::MoveToEnd, window, cx);
                });
                json!({ "filled": true })
            };
            if reply.send(answer).is_err() {
                log::debug!("the agent stopped waiting for the composer");
            }
        }
    }
}

/// Draws one plugin tree; plugin text is always plain text, never markdown or detected links.
struct Drawing<'a> {
    plugin_ui: &'a PluginUi,
    site: &'a Site,
    on_action: ActionHandler,
    /// wu's own element, placed at the tree's first `engine` node.
    own: Option<AnyElement>,
}

impl Drawing<'_> {
    fn node(&mut self, node: &UiNode, cx: &App) -> AnyElement {
        let colors = cx.theme().colors();
        match node {
            UiNode::Text { text, style } => div()
                .when(style.dim, |this| this.text_color(colors.text_muted))
                .when(style.bold, |this| this.font_weight(FontWeight::BOLD))
                .child(text.clone())
                .into_any_element(),
            UiNode::Unknown { text, .. } => div()
                .text_color(colors.text_muted)
                .child(text.clone())
                .into_any_element(),
            UiNode::Group {
                children,
                horizontal,
            } => {
                let children = children
                    .iter()
                    .map(|child| self.node(child, cx))
                    .collect::<Vec<_>>();
                if *horizontal {
                    h_flex().flex_wrap().gap(px(6.)).children(children)
                } else {
                    v_flex().min_w_0().children(children)
                }
                .into_any_element()
            }
            // shortcut: wu draws its row as it is even when the answer's props (a nonzero `ref`) rewrite it; apply those props once a plugin needs it.
            UiNode::Engine { .. } => self.own.take().unwrap_or_else(|| div().into_any_element()),
            UiNode::Button { label, target } => {
                let key = control_key(self.site, target);
                let on_action = self.on_action.clone();
                let target = target.clone();
                let selector = format!("plugin-ui-button-{}", target.key.as_deref().unwrap_or(""));
                div()
                    .debug_selector(|| selector)
                    .child(
                        Button::new(
                            SharedString::from(format!("plugin-ui-{key}")),
                            label.clone(),
                        )
                        .label_size(LabelSize::Small)
                        .on_click(move |_, window, cx| {
                            let action = UiAction::Press {
                                target: target.clone(),
                                href: None,
                            };
                            on_action(action, window, cx)
                        }),
                    )
                    .into_any_element()
            }
            UiNode::Link { label, href } => {
                let on_action = self.on_action.clone();
                let open = href.clone();
                div()
                    .id(SharedString::from(format!(
                        "plugin-ui-link-{}-{}-{href}",
                        self.site.0, self.site.1
                    )))
                    .text_color(colors.text_accent)
                    .cursor_pointer()
                    .tooltip(Tooltip::text(href.clone()))
                    .on_click(move |_, window, cx| {
                        on_action(UiAction::Open { href: open.clone() }, window, cx)
                    })
                    .child(label.clone())
                    .into_any_element()
            }
            UiNode::Input {
                label,
                placeholder,
                value,
                target,
            } => {
                let key = control_key(self.site, target);
                let field = match self.plugin_ui.inputs.get(&key) {
                    Some(input) => {
                        let editor = input.editor.clone();
                        let on_action = self.on_action.clone();
                        let target = target.clone();
                        div()
                            .debug_selector(|| format!("plugin-ui-input-{key}"))
                            .flex_1()
                            .min_w(px(120.))
                            .px(px(8.))
                            .py(px(4.))
                            .rounded(px(6.))
                            .border_1()
                            .border_color(colors.border)
                            .on_action(move |_: &menu::Confirm, window, cx| {
                                let value = editor.read(cx).text(cx);
                                let action = UiAction::Input {
                                    target: target.clone(),
                                    value,
                                    kind: Some("submit".into()),
                                };
                                on_action(action, window, cx)
                            })
                            .child(input.editor.clone())
                            .into_any_element()
                    }
                    None => div()
                        .text_color(colors.text_muted)
                        .child(if value.is_empty() {
                            placeholder.clone().unwrap_or_default()
                        } else {
                            value.clone().into()
                        })
                        .into_any_element(),
                };
                h_flex()
                    .gap(px(8.))
                    .children(label.clone())
                    .child(field)
                    .into_any_element()
            }
            UiNode::Select {
                label,
                value,
                options,
                target,
            } => {
                let key = control_key(self.site, target);
                let selected = self
                    .plugin_ui
                    .selections
                    .get(&key)
                    .or(value.as_ref())
                    .or(options.first().map(|option| &option.value))
                    .cloned();
                h_flex()
                    .flex_wrap()
                    .gap(px(4.))
                    .children(label.clone())
                    .children(options.iter().enumerate().map(|(index, option)| {
                        let on_action = self.on_action.clone();
                        let target = target.clone();
                        let value = option.value.clone();
                        Button::new(
                            ElementId::NamedInteger(
                                format!("plugin-ui-{key}").into(),
                                index as u64,
                            ),
                            option.label.clone(),
                        )
                        .label_size(LabelSize::Small)
                        .toggle_state(selected.as_ref() == Some(&option.value))
                        .on_click(move |_, window, cx| {
                            let action = UiAction::Select {
                                target: target.clone(),
                                value: value.clone(),
                            };
                            on_action(action, window, cx)
                        })
                    }))
                    .into_any_element()
            }
        }
    }
}

pub(super) fn caption(drawn_by: &[String], cx: &App) -> AnyElement {
    let text = match drawn_by {
        [plugin] => t!("From the {plugin} plugin", plugin = plugin),
        [others @ .., last] => t!(
            "From the {plugins} and {last} plugins",
            plugins = others.join(", "),
            last = last
        ),
        [] => return div().into_any_element(),
    };
    div()
        .debug_selector(|| "plugin-ui-caption".into())
        .pt(px(2.))
        .text_size(ui(11.))
        .text_color(text_faint(cx))
        .child(text)
        .into_any_element()
}

/// The CLI's own name and input for a tool call; wu keeps only what it shows, so fields it drops (a Bash `description`, say) are missing.
fn claude_tool(call: &ToolCall) -> (String, Value) {
    let (name, input) = match call {
        ToolCall::Exec { command } => ("Bash", json!({ "command": command })),
        ToolCall::ReadFile { path } => ("Read", json!({ "file_path": path })),
        ToolCall::WriteFile { path, content } => {
            ("Write", json!({ "file_path": path, "content": content }))
        }
        ToolCall::EditFile {
            path,
            old_string,
            new_string,
        } => (
            "Edit",
            json!({ "file_path": path, "old_string": old_string, "new_string": new_string }),
        ),
        ToolCall::ApplyPatch { path } => ("ApplyPatch", json!({ "path": path })),
        ToolCall::Search { pattern, path } => ("Grep", json!({ "pattern": pattern, "path": path })),
        ToolCall::Glob { pattern } => ("Glob", json!({ "pattern": pattern })),
        ToolCall::WebFetch { url, prompt } => ("WebFetch", json!({ "url": url, "prompt": prompt })),
        ToolCall::WebSearch { query } => ("WebSearch", json!({ "query": query })),
        ToolCall::Todo { items } => ("TodoWrite", json!({ "todos": items })),
        ToolCall::Mcp {
            server,
            tool,
            input,
        } => {
            return (
                format!("mcp__{server}__{tool}"),
                input.clone().unwrap_or_else(|| json!({})),
            );
        }
        ToolCall::Unknown { name, input } => {
            return (name.clone(), input.clone().unwrap_or_else(|| json!({})));
        }
    };
    (name.to_owned(), input)
}

fn tool_output(tool: &ToolEntry) -> Option<String> {
    let output = tool.output.as_ref()?;
    Some(output.chars().take(MAX_OUTPUT_CHARS).collect())
}

/// The extension's `ToolUse` props.
pub(super) fn tool_use_props(tool: &ToolEntry) -> Value {
    let (name, input) = claude_tool(&tool.call);
    let mut props = json!({
        "tool_use_id": tool.id,
        "tool": name,
        "input": input,
        "isRunning": tool.status == ToolStatus::Running,
        "isErrored": tool.status == ToolStatus::Failed,
        "isInterrupted": tool.status == ToolStatus::Canceled,
    });
    if tool.status != ToolStatus::Running
        && let Some(output) = tool_output(tool)
    {
        props["output"] = output.into();
    }
    props
}

/// The extension's `ToolResult` props.
pub(super) fn tool_result_props(tool: &ToolEntry) -> Value {
    json!({
        "tool_use_id": tool.id,
        "tool": claude_tool(&tool.call).0,
        "output": tool_output(tool),
        "isErrored": tool.status == ToolStatus::Failed,
    })
}

/// The extension's `ToolGroup` props.
pub(super) fn tool_group_props(entries: &[Entry], is_active: bool, is_expanded: bool) -> Value {
    let calls = entries
        .iter()
        .filter_map(|entry| match entry {
            Entry::Tool(tool) => Some(tool_use_props(tool)),
            _ => None,
        })
        .collect::<Vec<_>>();
    json!({ "calls": calls, "isActive": is_active, "isExpanded": is_expanded })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_tree(fixture: &str) -> Value {
        let mut frame: Value = serde_json::from_str(fixture).expect("fixture is JSON");
        frame["response"]["response"]["tree"].take()
    }

    fn count(node: &UiNode) -> usize {
        match node {
            UiNode::Group { children, .. } => 1 + children.iter().map(count).sum::<usize>(),
            _ => 1,
        }
    }

    fn depth(node: &UiNode) -> usize {
        match node {
            UiNode::Group { children, .. } => 1 + children.iter().map(depth).max().unwrap_or(0),
            _ => 1,
        }
    }

    #[test]
    fn parse_tree_reads_the_recorded_fixtures() {
        let tree = parse_tree(&fixture_tree(include_str!(
            "fixtures/plugin_ui/render-tool-use.json"
        )));
        let UiNode::Group {
            children,
            horizontal: false,
        } = tree
        else {
            panic!("expected a column, got {tree:?}");
        };
        assert_eq!(children[0], UiNode::Engine { reference: 1 });
        assert_eq!(
            children[1],
            UiNode::Text {
                text: "wu-draw-test line".into(),
                style: TextStyleHint {
                    dim: true,
                    bold: false
                },
            }
        );
        let UiNode::Button { label, target } = &children[2] else {
            panic!("expected a button, got {:?}", children[2]);
        };
        assert_eq!(label.as_ref(), "Ping");
        assert_eq!(target.plugin, "wu-draw-test");
        assert_eq!(target.key.as_deref(), Some("ping"));
        assert_eq!(children.len(), 3);

        let tree = parse_tree(&fixture_tree(include_str!(
            "fixtures/plugin_ui/render-pane.json"
        )));
        let UiNode::Group { children, .. } = tree else {
            panic!("expected a column, got {tree:?}");
        };
        assert!(matches!(&children[0], UiNode::Text { text, .. } if text.as_ref() == "Pane text"));
        assert_eq!(
            children[1],
            UiNode::Link {
                label: "Docs".into(),
                href: "https://example.com/docs".into()
            }
        );
        assert!(matches!(
            &children[2],
            UiNode::Input { label: Some(label), placeholder: Some(placeholder), value, target }
                if label.as_ref() == "Name" && placeholder.as_ref() == "type here" && value.is_empty() && target.key.as_deref() == Some("name")
        ));
        let UiNode::Select { value, options, .. } = &children[3] else {
            panic!("expected a select, got {:?}", children[3]);
        };
        assert_eq!(value.as_deref(), Some("red"));
        assert_eq!(
            options,
            &vec![
                SelectOption {
                    value: "red".into(),
                    label: "Red".into()
                },
                SelectOption {
                    value: "blue".into(),
                    label: "blue".into()
                },
            ]
        );
    }

    #[test]
    fn unknown_node_kinds_become_text_not_errors() {
        let tree = parse_tree(&json!({
            "type": "Box",
            "props": {"flexDirection": "row", "futureProp": [1, 2]},
            "part": {"plugin": "outer"},
            "children": [
                {"type": "Markdown", "props": {"text": "**hi**"}, "press": {"plugin": "p", "handle": 1}},
                {"type": "div", "children": ["a", {"type": "b", "children": ["b"]}, {"type": "Button", "props": {"key": "go", "label": "Go"}, "press": {"plugin": "p", "handle": 7}}], "part": {"plugin": "inner"}},
                {"type": "Client", "children": ["client"]},
                {"type": "Button", "props": {"label": "No press"}},
                {"type": "Button", "props": {"label": "Bad handle"}, "press": {"plugin": "p", "handle": "1"}},
                {"type": "Text", "children": ["x", {"type": "Text", "children": ["y"]}]},
                42,
                null,
            ],
        }));
        let UiNode::Group {
            children,
            horizontal: true,
        } = tree
        else {
            panic!("expected a row, got {tree:?}");
        };
        let unknown = |kind: &str, text: &str| UiNode::Unknown {
            kind: kind.into(),
            text: text.to_owned().into(),
        };
        assert_eq!(
            children,
            vec![
                unknown("Markdown", "**hi**"),
                UiNode::Group {
                    children: vec![
                        UiNode::Text {
                            text: "a".into(),
                            style: TextStyleHint::default()
                        },
                        UiNode::Text {
                            text: "b".into(),
                            style: TextStyleHint {
                                dim: false,
                                bold: true
                            }
                        },
                        UiNode::Button {
                            label: "Go".into(),
                            target: Target {
                                plugin: "p".into(),
                                handle: 7.into(),
                                key: Some("go".into())
                            }
                        },
                    ],
                    horizontal: true
                },
                unknown("Client", "client"),
                unknown("Button", "No press"),
                unknown("Button", "Bad handle"),
                UiNode::Text {
                    text: "xy".into(),
                    style: TextStyleHint::default()
                },
                unknown("", ""),
                unknown("", ""),
            ]
        );
        assert_eq!(
            parse_tree(&json!("plain")),
            UiNode::Text {
                text: "plain".into(),
                style: TextStyleHint::default()
            }
        );
    }

    #[test]
    fn oversized_and_deeply_nested_trees_are_capped() {
        let wide = json!({
            "type": "Box",
            "children": (0..10_000).map(|index| json!({"type": "Text", "children": [index.to_string()]})).collect::<Vec<_>>(),
        });
        assert!(count(&parse_tree(&wide)) <= MAX_NODES);

        // `json!` serializes an interpolated value recursively, so nesting is built by hand.
        let wrap = |kind: &str, child: Value| {
            let mut node = json!({"type": kind, "props": {"flexDirection": "column"}});
            node["children"] = Value::Array(vec![child]);
            node
        };
        let mut deep = json!({"type": "Text", "children": ["leaf"]});
        for _ in 0..200 {
            deep = wrap("Box", deep);
        }
        let tree = parse_tree(&deep);
        assert_eq!(depth(&tree), MAX_DEPTH);
        let mut node = &tree;
        while let UiNode::Group { children, .. } = node {
            node = &children[0];
        }
        assert_eq!(
            node,
            &UiNode::Unknown {
                kind: "Box".into(),
                text: "leaf".into()
            }
        );

        let mut very_deep = json!("leaf");
        for _ in 0..100_000 {
            very_deep = wrap("Text", very_deep);
        }
        assert!(matches!(parse_tree(&very_deep), UiNode::Text { .. }));
        // Dropping a value this deep recurses, so take it apart by hand.
        let mut rest = very_deep;
        while let Some(child) = rest
            .get_mut("children")
            .and_then(|children| children.get_mut(0))
            .map(Value::take)
        {
            rest = child;
        }
    }

    #[test]
    fn not_available_without_the_capability() {
        assert!(!PluginUi::available(&[]));
        assert!(!PluginUi::available(&["msg_lifecycle_v1".into()]));
        assert!(PluginUi::available(&[
            "msg_lifecycle_v1".into(),
            "ui_surface_v1".into()
        ]));
    }

    fn cached(plugin_ui: &mut PluginUi, component: &str, instance_id: &str) {
        plugin_ui.cache.insert(
            (component.into(), instance_id.into()),
            Rendered::from_answer(&json!({"tree": {"type": "Text", "children": ["x"]}})),
        );
    }

    #[test]
    fn invalidate_marks_only_listed_instances_stale_and_keeps_them_drawn() {
        let mut plugin_ui = PluginUi::new();
        cached(&mut plugin_ui, "ToolUse", "toolu_1");
        cached(&mut plugin_ui, "ToolUse", "toolu_2");
        cached(&mut plugin_ui, "Pane", "pane");
        plugin_ui.unhooked.insert("Spinner".into());

        assert!(plugin_ui.apply_event(
            &json!({
                "subtype": "ui_invalidate",
                "instances": [{"surface": SURFACE, "component": "ToolUse", "instance_id": "toolu_1"}],
            }),
            Instant::now()
        ));
        assert_eq!(
            plugin_ui.stale.iter().cloned().collect::<Vec<_>>(),
            vec![("ToolUse".to_owned(), "toolu_1".to_owned())]
        );
        assert_eq!(
            plugin_ui.cache.len(),
            3,
            "a stale drawing stays until the new one arrives"
        );
        assert!(
            plugin_ui.unhooked.is_empty(),
            "an invalidation asks unhooked components again"
        );

        assert!(plugin_ui.apply_event(&json!({"subtype": "ui_invalidate"}), Instant::now()));
        assert_eq!(plugin_ui.stale.len(), 3);
    }

    #[test]
    fn follow_run_keeps_counting_requests() {
        let mut plugin_ui = PluginUi::new();
        plugin_ui.attaches_sent = 3;
        plugin_ui.requests_sent = 7;
        plugin_ui.follow_run(1);
        assert_eq!((plugin_ui.attaches_sent, plugin_ui.requests_sent), (3, 7));
    }

    #[test]
    fn non_http_hrefs_are_not_opened() {
        for href in [
            "file:///C:/Windows/System32/calc.exe",
            "javascript:alert(1)",
            "wu://open?path=secret",
            "mailto:someone@example.com",
            " https://example.com",
            "example.com",
            "http:example.com",
            "javascript:alert(1)//://",
            "https://exa mple.com",
            "https://example.com/\n",
            "https://example.com/\u{7}",
        ] {
            assert!(!opens_href(href), "{href} must not open");
        }
        for href in [
            "https://example.com/docs",
            "http://localhost:3000",
            "HTTPS://EXAMPLE.COM",
        ] {
            assert!(opens_href(href), "{href} opens");
        }
    }

    #[test]
    fn drawings_show_only_when_a_plugin_drew_something() {
        let rendered = |answer: Value| Rendered::from_answer(&answer).shows();
        let tree = json!({"type": "Box", "children": [{"type": "engine", "ref": 1}, {"type": "Text", "children": ["line"]}]});
        assert!(rendered(
            json!({"tree": tree, "hooked": true, "drawn_by": ["p"]})
        ));
        assert!(!rendered(
            json!({"tree": tree, "hooked": false, "drawn_by": ["p"]})
        ));
        assert!(!rendered(
            json!({"tree": tree, "hooked": true, "drawn_by": []})
        ));
        assert!(!rendered(
            json!({"tree": {"type": "Box", "children": [{"type": "engine", "ref": 0}, " "]}, "hooked": true, "drawn_by": ["p"]})
        ));
    }

    #[test]
    fn status_null_clears_and_toasts_expire() {
        let mut plugin_ui = PluginUi::new();
        assert!(plugin_ui.apply_event(
            &json!({"subtype": "ui_status", "plugin": "a", "text": "busy"}),
            Instant::now()
        ));
        assert!(plugin_ui.apply_event(
            &json!({"subtype": "ui_status", "plugin": "b", "text": "idle"}),
            Instant::now()
        ));
        assert!(plugin_ui.apply_event(
            &json!({"subtype": "ui_status", "plugin": "a", "text": null}),
            Instant::now()
        ));
        assert_eq!(
            plugin_ui.statuses.iter().collect::<Vec<_>>(),
            vec![(&"b".to_owned(), &SharedString::from("idle"))]
        );

        for index in 0..7 {
            plugin_ui.apply_event(
                &json!({
                    "subtype": "ui_toast",
                    "plugin": "a",
                    "text": format!("toast {index}"),
                    "timeout_ms": if index == 6 { 60_000 } else { 1_000 },
                }),
                Instant::now(),
            );
        }
        assert_eq!(
            plugin_ui
                .toasts
                .iter()
                .map(|toast| toast.text.as_ref())
                .collect::<Vec<_>>(),
            vec!["toast 2", "toast 3", "toast 4", "toast 5", "toast 6"]
        );
        assert!(!plugin_ui.expire_toasts(Instant::now()));
        assert!(plugin_ui.expire_toasts(Instant::now() + Duration::from_secs(2)));
        assert_eq!(plugin_ui.toasts.len(), 1);
        assert_eq!(plugin_ui.toasts[0].text.as_ref(), "toast 6");

        plugin_ui.follow_run(1);
        assert!(plugin_ui.statuses.is_empty() && plugin_ui.toasts.is_empty());
    }

    fn roster(panes: Value, shown_id: Value) -> Value {
        json!({ "subtype": "ui_panes", "panes": panes, "shown_id": shown_id, "focused_id": null, "focus_requested_id": null })
    }

    fn pane_ids(plugin_ui: &PluginUi) -> Vec<&str> {
        plugin_ui
            .panes
            .iter()
            .map(|pane| pane.id.as_str())
            .collect()
    }

    #[test]
    fn shown_id_selects_the_visible_pane() {
        let mut plugin_ui = PluginUi::new();
        assert!(plugin_ui.visible_pane().is_none());
        let panes = json!([
            { "id": "a", "plugin": "p", "title": "A" },
            { "id": "b", "plugin": "p", "title": "B" },
        ]);
        let visible = |plugin_ui: &PluginUi| plugin_ui.visible_pane().map(|pane| pane.id.clone());
        plugin_ui.apply_event(&roster(panes.clone(), json!("b")), Instant::now());
        assert_eq!(visible(&plugin_ui).as_deref(), Some("b"));
        plugin_ui.apply_event(&roster(panes.clone(), json!("gone")), Instant::now());
        assert_eq!(
            visible(&plugin_ui).as_deref(),
            Some("a"),
            "an unknown shown id falls back to the first pane"
        );
        plugin_ui.apply_event(&roster(panes, Value::Null), Instant::now());
        assert_eq!(visible(&plugin_ui).as_deref(), Some("a"));
    }

    #[test]
    fn invalid_background_color_is_ignored() {
        let mut plugin_ui = PluginUi::new();
        plugin_ui.apply_event(
            &roster(
                json!([
                    { "id": "a", "plugin": "p", "background_color": "#ff000080" },
                    { "id": "b", "plugin": "p", "background_color": "red" },
                    { "id": "c", "plugin": "p", "background_color": "#zzzzzz" },
                    { "id": "d", "plugin": "p", "background_color": 7 },
                    { "id": "e", "plugin": "p", "title": 5, "rows": 99_999_999_999u64, "columns": -3 },
                    { "plugin": "p", "title": "no id" },
                    { "id": "f", "title": "no plugin" },
                    "junk",
                ]),
                json!(42),
            ),
            Instant::now(),
        );
        assert_eq!(pane_ids(&plugin_ui), ["a", "b", "c", "d", "e"]);
        let backgrounds = plugin_ui
            .panes
            .iter()
            .map(|pane| pane.background.is_some())
            .collect::<Vec<_>>();
        assert_eq!(backgrounds, [true, false, false, false, false]);
        let malformed = &plugin_ui.panes[4];
        assert_eq!(malformed.title, "e", "a pane without a title shows its id");
        assert_eq!((malformed.rows, malformed.columns), (None, None));
        assert_eq!(plugin_ui.shown_pane, None);
    }

    #[test]
    fn pane_size_is_clamped_to_the_side_pane() {
        let pane = |rows: Option<u32>, columns: Option<u32>| PaneInfo {
            id: "a".into(),
            plugin: "p".into(),
            title: "A".into(),
            close_on_escape: false,
            background: None,
            rows,
            columns,
        };
        assert_eq!(pane_cells(&pane(None, None), 80, 40), (80, 40));
        assert_eq!(pane_cells(&pane(Some(5), Some(500)), 80, 40), (80, 5));
        assert_eq!(pane_cells(&pane(Some(0), Some(u32::MAX)), 80, 40), (80, 1));
    }
}
