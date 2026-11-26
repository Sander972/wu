// shortcut: Tasks 48-49 draw the trees, panes, statuses and toasts kept here; drop this allow once they read them.
#![allow(dead_code)]

use std::collections::{BTreeMap, VecDeque};
use std::time::{Duration, Instant};

use anyhow::Result;
use collections::HashMap;
use editor::MultiBufferOffset;
use gpui::{App, AppContext as _, Context, Entity, SharedString, Task, TaskExt as _, Window};
use serde_json::{Value, json};
use util::ResultExt as _;

use super::ChatView;
use crate::{AgentKind, session::AgentSession};

/// Temporary, user-approved stand-in: the CLI only accepts the "desktop", "mobile" and "vscode" surfaces.
pub(crate) const SURFACE: &str = "vscode";
const CAPABILITY: &str = "ui_surface_v1";
const MAX_DEPTH: usize = 64;
const MAX_NODES: usize = 5_000;
const MAX_TOASTS: usize = 5;
const MAX_DRAWN_BY: usize = 20;
/// What the CLI sent for a toast without its own timeout in the recording.
const DEFAULT_TOAST_TIMEOUT: Duration = Duration::from_millis(4000);

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
        "Box" if depth < MAX_DEPTH => {
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
        "Text" => UiNode::Text {
            text: text_content(node, budget),
            style: TextStyleHint {
                dim: prop_flag(node, "dimColor"),
                bold: prop_flag(node, "bold"),
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
    pub rows: Option<u32>,
    pub columns: Option<u32>,
}

pub(crate) struct Toast {
    pub plugin: String,
    pub text: SharedString,
    pub expires_at: Instant,
}

pub(crate) struct Rendered {
    pub tree: UiNode,
    /// The props an `Engine` node with a nonzero reference draws with.
    pub props: Value,
    pub drawn_by: Vec<String>,
    pub rewritten: bool,
}

impl Rendered {
    fn from_answer(answer: &Value) -> Self {
        Self {
            tree: parse_tree(&answer["tree"]),
            props: answer["props"].clone(),
            drawn_by: answer["drawn_by"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .take(MAX_DRAWN_BY)
                .map(str::to_owned)
                .collect(),
            rewritten: answer["rewritten"].as_bool().unwrap_or_default(),
        }
    }
}

/// One chat view's attachment to its agent's plugin drawing, and what the plugins drew so far in the current run.
pub(crate) struct PluginUi {
    client_id: String,
    /// The run this state belongs to; each run is a new agent process that knows nothing of the last one.
    run: u64,
    attached: bool,
    pub statuses: BTreeMap<String, SharedString>,
    pub toasts: VecDeque<Toast>,
    pub panes: Vec<PaneInfo>,
    pub shown_pane: Option<String>,
    cache: HashMap<(String, String), Rendered>,
    /// Renders asked for and not cached, with the number of the latest request; a failed one stays here so it isn't asked again every frame.
    requested: HashMap<(String, String), u64>,
    requests_sent: u64,
}

impl PluginUi {
    pub fn new() -> Self {
        Self {
            client_id: uuid::Uuid::new_v4().to_string(),
            run: 0,
            attached: false,
            statuses: BTreeMap::new(),
            toasts: VecDeque::new(),
            panes: Vec::new(),
            shown_pane: None,
            cache: HashMap::default(),
            requested: HashMap::default(),
            requests_sent: 0,
        }
    }

    pub fn available(capabilities: &[String]) -> bool {
        capabilities
            .iter()
            .any(|capability| capability == CAPABILITY)
    }

    /// Forgets what an earlier run drew once `run` is a different one.
    pub fn follow_run(&mut self, run: u64) {
        if self.run != run {
            *self = Self {
                client_id: std::mem::take(&mut self.client_id),
                run,
                ..Self::new()
            };
        }
    }

    fn wants_attach(&self, session: &AgentSession) -> bool {
        !self.attached
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
        // A refused attach is not retried until the chat is shown again, so a refusing agent isn't asked every frame.
        self.attached = true;
        let request = json!({
            "subtype": "ui_attach",
            "surface": SURFACE,
            "client_id": self.client_id,
            "viewport": { "columns": columns.max(1), "rows": rows.max(1), "isFullscreen": false },
            "answers": ["ui_copy", "ui_prompt_read", "ui_prompt_fill", "ui_prompt_suggest"],
        });
        let answer = session.update(cx, |session, cx| session.control(request, cx));
        cx.background_spawn(async move {
            answer.await?;
            Ok(())
        })
    }

    pub fn detach(&mut self, session: &Entity<AgentSession>, cx: &mut App) {
        self.follow_run(session.read(cx).runs_started());
        if !std::mem::take(&mut self.attached) {
            return;
        }
        self.cache.clear();
        self.requested.clear();
        // Without a live run a control request would start a one-off agent process just to detach.
        if !session.read(cx).has_live_run() {
            return;
        }
        let request = json!({ "subtype": "ui_detach", "client_id": self.client_id });
        session
            .update(cx, |session, cx| session.control(request, cx))
            .detach_and_log_err(cx);
    }

    /// The cached drawing of a component instance; when there is none yet it is asked for in the background and the view notified once it arrives.
    pub fn render(
        &mut self,
        session: &Entity<AgentSession>,
        component: &str,
        instance_id: &str,
        props: Value,
        cx: &mut Context<ChatView>,
    ) -> Option<&Rendered> {
        let key = (component.to_owned(), instance_id.to_owned());
        if self.cache.contains_key(&key) {
            return self.cache.get(&key);
        }
        if !self.attached || self.requested.contains_key(&key) {
            return None;
        }
        self.requests_sent += 1;
        let number = self.requests_sent;
        self.requested.insert(key.clone(), number);
        let request = json!({
            "subtype": "ui_render",
            "surface": SURFACE,
            "client_id": self.client_id,
            "component": component,
            "instance_id": instance_id,
            "props": props,
        });
        let answer = session.update(cx, |session, cx| session.control(request, cx));
        cx.spawn(async move |this, cx| {
            let answer = answer.await;
            this.update(cx, |view, cx| {
                let plugin_ui = &mut view.plugin_ui;
                // An invalidation or a newer request made this answer stale.
                if plugin_ui.requested.get(&key) != Some(&number) {
                    return;
                }
                match answer {
                    Ok(answer) => {
                        plugin_ui.requested.remove(&key);
                        plugin_ui.cache.insert(key, Rendered::from_answer(&answer));
                        cx.notify();
                    }
                    Err(error) => log::warn!("the agent didn't draw {}: {error:#}", key.0),
                }
            })
            .log_err();
        })
        .detach();
        None
    }

    /// Applies a drawing frame from the agent; true when the view must draw again.
    pub fn apply_event(&mut self, event: &Value) -> bool {
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
                    expires_at: Instant::now() + timeout,
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
                            self.cache.remove(&key);
                            self.requested.remove(&key);
                        }
                    }
                    None => {
                        self.cache.clear();
                        self.requested.clear();
                    }
                }
                true
            }
            "ui_panes" => {
                self.panes = event["panes"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|pane| {
                        let dimension = |name: &str| {
                            pane[name]
                                .as_u64()
                                .and_then(|value| u32::try_from(value).ok())
                        };
                        Some(PaneInfo {
                            id: pane["id"].as_str()?.to_owned(),
                            plugin: pane["plugin"].as_str().unwrap_or_default().to_owned(),
                            title: pane["title"].as_str().unwrap_or_default().to_owned(),
                            close_on_escape: pane["close_on_escape"].as_bool().unwrap_or_default(),
                            rows: dimension("rows"),
                            columns: dimension("columns"),
                        })
                    })
                    .collect();
                self.shown_pane = event["shown_id"].as_str().map(str::to_owned);
                true
            }
            _ => false,
        }
    }

    /// Drops the toasts whose time is up at `now`; true when any went.
    pub fn expire_toasts(&mut self, now: Instant) -> bool {
        let before = self.toasts.len();
        self.toasts.retain(|toast| toast.expires_at > now);
        self.toasts.len() != before
    }
}

impl ChatView {
    /// Attaches from draw time, since only a frame being drawn proves the chat is visible; `Item::deactivated` detaches.
    pub(super) fn sync_plugin_ui(&mut self, window: &Window, cx: &mut Context<Self>) {
        let session = self.session.read(cx);
        self.plugin_ui.follow_run(session.runs_started());
        if !self.plugin_ui.wants_attach(session) {
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
        if self.plugin_ui.apply_event(event) {
            cx.notify();
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
                {"type": "div", "children": ["a", {"type": "span", "children": ["b"]}], "part": {"plugin": "inner"}},
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
                unknown("div", "ab"),
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
    fn invalidate_drops_only_listed_instances() {
        let mut plugin_ui = PluginUi::new();
        cached(&mut plugin_ui, "ToolUse", "toolu_1");
        cached(&mut plugin_ui, "ToolUse", "toolu_2");
        cached(&mut plugin_ui, "Pane", "pane");

        assert!(plugin_ui.apply_event(&json!({
            "subtype": "ui_invalidate",
            "instances": [{"surface": SURFACE, "component": "ToolUse", "instance_id": "toolu_1"}],
        })));
        let mut keys: Vec<_> = plugin_ui.cache.keys().cloned().collect();
        keys.sort();
        assert_eq!(
            keys,
            vec![
                ("Pane".to_owned(), "pane".to_owned()),
                ("ToolUse".to_owned(), "toolu_2".to_owned())
            ]
        );

        assert!(plugin_ui.apply_event(&json!({"subtype": "ui_invalidate"})));
        assert!(plugin_ui.cache.is_empty());
    }

    #[test]
    fn status_null_clears_and_toasts_expire() {
        let mut plugin_ui = PluginUi::new();
        assert!(
            plugin_ui.apply_event(&json!({"subtype": "ui_status", "plugin": "a", "text": "busy"}))
        );
        assert!(
            plugin_ui.apply_event(&json!({"subtype": "ui_status", "plugin": "b", "text": "idle"}))
        );
        assert!(
            plugin_ui.apply_event(&json!({"subtype": "ui_status", "plugin": "a", "text": null}))
        );
        assert_eq!(
            plugin_ui.statuses.iter().collect::<Vec<_>>(),
            vec![(&"b".to_owned(), &SharedString::from("idle"))]
        );

        for index in 0..7 {
            plugin_ui.apply_event(&json!({
                "subtype": "ui_toast",
                "plugin": "a",
                "text": format!("toast {index}"),
                "timeout_ms": if index == 6 { 60_000 } else { 1_000 },
            }));
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
}
