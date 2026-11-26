use super::{ChatView, composer::open_path};
use crate::{session::AgentSession, slash_commands::AppCommand};
use editor::Editor;
use gpui::{
    App, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable, PromptButton,
    PromptLevel, Render, ScrollHandle, SharedString, Task, WeakEntity, Window, rems,
};
use i18n::t;
use serde_json::{Value, json};
use std::path::PathBuf;
use ui::{
    Modal, ModalFooter, ModalHeader, Section, SectionHeader, Switch, ToggleButtonGroup,
    ToggleButtonSimple, ToggleState, prelude::*,
};
use util::ResultExt as _;
use workspace::{ModalView, Toast, Workspace, notifications::NotificationId};

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct DialogSection {
    pub title: SharedString,
    pub rows: Vec<DialogRow>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct DialogRow {
    pub label: SharedString,
    pub detail: Option<SharedString>,
    pub badge: Option<SharedString>,
    pub actions: Vec<RowAction>,
}

#[derive(Clone, Debug, PartialEq)]
#[expect(
    dead_code,
    reason = "OpenUrl rows come with the dialogs that need them"
)]
pub(crate) enum RowAction {
    OpenPath(PathBuf),
    OpenUrl(String),
    Control {
        label: SharedString,
        request: Value,
    },
    Toggle {
        on: bool,
        request_on: Value,
        request_off: Value,
    },
    Shell {
        label: SharedString,
        arguments: Vec<String>,
    },
    Authenticate {
        label: SharedString,
        request: Value,
    },
}

impl DialogRow {
    fn new(label: impl Into<SharedString>) -> Self {
        Self {
            label: label.into(),
            detail: None,
            badge: None,
            actions: Vec::new(),
        }
    }
}

fn text(value: &Value, key: &str) -> Option<SharedString> {
    value[key]
        .as_str()
        .filter(|text| !text.is_empty())
        .map(|text| SharedString::from(text.to_string()))
}

fn items<'a>(value: &'a Value, key: &str) -> impl Iterator<Item = &'a Value> {
    value[key].as_array().into_iter().flatten()
}

fn yes_no(value: bool) -> SharedString {
    if value { t!("Yes") } else { t!("No") }
}

fn push_grouped(sections: &mut Vec<DialogSection>, title: SharedString, row: DialogRow) {
    match sections.iter_mut().find(|section| section.title == title) {
        Some(section) => section.rows.push(row),
        None => sections.push(DialogSection {
            title,
            rows: vec![row],
        }),
    }
}

pub(crate) fn status_sections(response: &Value) -> Vec<DialogSection> {
    items(response, "sections")
        .map(|section| DialogSection {
            title: text(section, "title").unwrap_or_default(),
            rows: items(section, "rows")
                .map(|row| match text(row, "label") {
                    Some(label) => DialogRow {
                        detail: text(row, "value"),
                        ..DialogRow::new(label)
                    },
                    None => DialogRow::new(text(row, "value").unwrap_or_default()),
                })
                .collect(),
        })
        .collect()
}

pub(crate) fn hooks_sections(response: &Value) -> Vec<DialogSection> {
    let mut sections = Vec::new();
    for hook in items(response, "hooks") {
        let event = text(hook, "event").unwrap_or_default();
        let summary = items(response, "events")
            .find(|candidate| candidate["name"] == event.as_ref())
            .and_then(|candidate| text(candidate, "summary"));
        let title = match summary {
            Some(summary) => format!("{event} · {summary}").into(),
            None => event,
        };
        let row = DialogRow {
            detail: text(hook, "displayText").or_else(|| text(hook, "commandText")),
            badge: text(hook, "pluginName")
                .or_else(|| text(hook, "sourceLabel"))
                .or_else(|| text(hook, "source")),
            ..DialogRow::new(text(hook, "matcher").unwrap_or_else(|| "*".into()))
        };
        push_grouped(&mut sections, title, row);
    }
    sections
}

pub(crate) fn memory_sections(response: &Value) -> Vec<DialogSection> {
    let open = |entry: &Value| -> Vec<RowAction> {
        entry["path"]
            .as_str()
            .map(|path| RowAction::OpenPath(path.into()))
            .into_iter()
            .collect()
    };
    let files: Vec<DialogRow> = items(response, "files")
        .map(|file| DialogRow {
            detail: text(file, "description"),
            badge: (file["exists"] == false).then(|| t!("Not created yet")),
            actions: open(file),
            ..DialogRow::new(
                text(file, "label")
                    .or_else(|| text(file, "path"))
                    .unwrap_or_default(),
            )
        })
        .collect();
    let mut sections = Vec::new();
    if !files.is_empty() {
        sections.push(DialogSection {
            title: t!("Memory files"),
            rows: files,
        });
    }
    let mut memories = Vec::new();
    let auto_memory = &response["auto_memory"];
    if auto_memory.is_object() {
        let mut row = DialogRow {
            detail: text(auto_memory, "status"),
            ..DialogRow::new(t!("Save memories automatically"))
        };
        if auto_memory["toggleable"] == true {
            let setting = |enabled: bool| {
                json!({
                    "subtype": "update_settings",
                    "source": "userSettings",
                    "settings": { "autoMemoryEnabled": enabled },
                })
            };
            row.actions.push(RowAction::Toggle {
                on: auto_memory["enabled"] == true,
                request_on: setting(true),
                request_off: setting(false),
            });
        }
        memories.push(row);
    }
    memories.extend(items(response, "memories").map(|memory| {
        DialogRow {
            detail: text(memory, "description"),
            actions: open(memory),
            ..DialogRow::new(
                text(memory, "name")
                    .or_else(|| text(memory, "path"))
                    .unwrap_or_default(),
            )
        }
    }));
    if !memories.is_empty() {
        sections.push(DialogSection {
            title: t!("Auto memory"),
            rows: memories,
        });
    }
    sections
}

pub(crate) fn skills_sections(response: &Value) -> Vec<DialogSection> {
    let mut sections = Vec::new();
    for skill in items(response, "skills") {
        let row = DialogRow {
            detail: text(skill, "description"),
            badge: text(skill, "state"),
            ..DialogRow::new(
                text(skill, "display_name")
                    .or_else(|| text(skill, "name"))
                    .unwrap_or_default(),
            )
        };
        push_grouped(
            &mut sections,
            text(skill, "source").unwrap_or_default(),
            row,
        );
    }
    sections
}

pub(crate) fn sandbox_sections(response: &Value) -> Vec<DialogSection> {
    let supported = response["supported"] == true;
    let supported = match text(response, "unsupported_reason").filter(|_| !supported) {
        Some(reason) => format!("{} ({reason})", yes_no(false)).into(),
        None => yes_no(supported),
    };
    let state = [
        (t!("Supported"), Some(supported)),
        (t!("Enabled"), Some(yes_no(response["enabled"] == true))),
        (t!("Mode"), text(response, "mode")),
    ]
    .map(|(label, detail)| DialogRow {
        detail,
        ..DialogRow::new(label)
    });
    let mut sections = vec![DialogSection {
        title: t!("Sandbox"),
        rows: state.into(),
    }];
    let excluded: Vec<DialogRow> = items(response, "excluded_commands")
        .filter_map(Value::as_str)
        .map(|command| DialogRow::new(command.to_string()))
        .collect();
    if !excluded.is_empty() {
        sections.push(DialogSection {
            title: t!("Excluded commands"),
            rows: excluded,
        });
    }
    let restrictions: Vec<DialogRow> = response["restrictions"]
        .as_object()
        .into_iter()
        .flatten()
        .filter_map(|(name, values)| {
            let values: Vec<&str> = values
                .as_array()?
                .iter()
                .filter_map(Value::as_str)
                .collect();
            (!values.is_empty()).then(|| DialogRow {
                detail: Some(values.join(", ").into()),
                ..DialogRow::new(name.clone())
            })
        })
        .collect();
    if !restrictions.is_empty() {
        sections.push(DialogSection {
            title: t!("Restrictions"),
            rows: restrictions,
        });
    }
    sections
}

const MCP_TRANSPORTS: [&str; 3] = ["http", "sse", "stdio"];
const MCP_SCOPES: [&str; 3] = ["local", "user", "project"];

pub(crate) fn mcp_sections(response: &Value) -> Vec<DialogSection> {
    let mut sections = Vec::new();
    for server in items(response, "mcpServers") {
        let Some(name) = text(server, "name") else {
            continue;
        };
        let name_text: &str = &name;
        let status = text(server, "status").unwrap_or_default();
        let named = |subtype: &str| json!({ "subtype": subtype, "serverName": name_text });
        let control = |label: SharedString, request: Value| RowAction::Control { label, request };
        let toggle = |label: SharedString, enabled: bool| {
            let mut request = named("mcp_toggle");
            request["enabled"] = json!(enabled);
            control(label, request)
        };
        let mut actions = match status.as_ref() {
            "connected" => {
                let mut actions = vec![
                    toggle(t!("Disable"), false),
                    control(t!("Reconnect"), named("mcp_reconnect")),
                ];
                if matches!(server["config"]["type"].as_str(), Some("http" | "sse")) {
                    actions.push(control(t!("Clear authentication"), named("mcp_clear_auth")));
                }
                actions
            }
            "disabled" => vec![toggle(t!("Enable"), true)],
            "failed" => vec![control(t!("Reconnect"), named("mcp_reconnect"))],
            "needs-auth" => vec![RowAction::Authenticate {
                label: t!("Authenticate"),
                request: named("mcp_authenticate"),
            }],
            _ => Vec::new(),
        };
        let scope = text(server, "scope");
        if let Some(scope) = scope
            .as_ref()
            .filter(|scope| MCP_SCOPES.contains(&scope.as_ref()))
        {
            actions.push(RowAction::Shell {
                label: t!("Remove"),
                arguments: ["mcp", "remove", "--scope", scope, name_text]
                    .map(String::from)
                    .into(),
            });
        }
        let tool_count = items(server, "tools").count();
        let row = DialogRow {
            detail: text(server, "error")
                .or_else(|| (tool_count > 0).then(|| t!("{count} tools", count = tool_count))),
            badge: Some(status.clone()),
            actions,
            ..DialogRow::new(name.clone())
        };
        push_grouped(&mut sections, scope.unwrap_or_else(|| t!("Servers")), row);
    }
    sections
}

fn add_server_arguments(transport: &str, scope: &str, name: &str, target: &str) -> Vec<String> {
    [
        "mcp",
        "add",
        "--transport",
        transport,
        "--scope",
        scope,
        name,
        target,
    ]
    .map(String::from)
    .into()
}

/// The CLI would read a leading dash as one of its own options, since the arguments carry no `--` separator.
fn server_input_error(name: &str, target: &str) -> Option<SharedString> {
    if name.is_empty() {
        Some(t!("Enter a server name."))
    } else if target.is_empty() {
        Some(t!("Enter a URL or a command."))
    } else if name.starts_with('-') || target.starts_with('-') {
        Some(t!("The name and the address can't start with a dash."))
    } else {
        None
    }
}

fn authentication_url(response: &Value) -> Option<String> {
    text(response, "authUrl")
        .filter(|url| url.starts_with("https://") || url.starts_with("http://"))
        .map(|url| url.to_string())
}

fn output_style_sections(styles: &Value, settings: &Value, live_run: bool) -> Vec<DialogSection> {
    let current = settings["effective"]["outputStyle"]
        .as_str()
        .unwrap_or("default");
    let note = (!live_run).then(|| {
        DialogRow::new(t!(
            "Start a turn first: the output style applies to the running Claude session."
        ))
    });
    let styles = items(styles, "available_output_styles")
        .filter_map(Value::as_str)
        .map(|style| {
            let mut row = DialogRow::new(style.to_string());
            if style == current {
                row.badge = Some(t!("Current"));
            } else if live_run {
                row.actions.push(RowAction::Control {
                    label: t!("Use"),
                    request: json!({
                        "subtype": "apply_flag_settings",
                        "settings": { "outputStyle": style },
                    }),
                });
            }
            row
        });
    vec![DialogSection {
        title: t!("Styles"),
        rows: note.into_iter().chain(styles).collect(),
    }]
}

type Reload = Box<dyn Fn(&mut App) -> Task<anyhow::Result<Vec<DialogSection>>>>;

struct AddServerForm {
    name: Entity<Editor>,
    target: Entity<Editor>,
    transport: &'static str,
    scope: &'static str,
    error: Option<SharedString>,
}

enum DialogState {
    Loading,
    Failed(SharedString),
    Loaded(Vec<DialogSection>),
}

pub(crate) struct ClaudeDialog {
    title: SharedString,
    session: Entity<AgentSession>,
    workspace: WeakEntity<Workspace>,
    reload: Reload,
    state: DialogState,
    can_add_server: bool,
    form: Option<AddServerForm>,
    focus_handle: FocusHandle,
    scroll_handle: ScrollHandle,
    _pending: Task<()>,
}

impl EventEmitter<DismissEvent> for ClaudeDialog {}
impl ModalView for ClaudeDialog {}

impl Focusable for ClaudeDialog {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl ClaudeDialog {
    fn new(
        title: SharedString,
        session: Entity<AgentSession>,
        workspace: WeakEntity<Workspace>,
        reload: Reload,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut dialog = Self {
            title,
            session,
            workspace,
            reload,
            state: DialogState::Loading,
            can_add_server: false,
            form: None,
            focus_handle: cx.focus_handle(),
            scroll_handle: ScrollHandle::new(),
            _pending: Task::ready(()),
        };
        dialog.reload(cx);
        dialog
    }

    fn for_control(
        title: SharedString,
        request: Value,
        sections: fn(&Value) -> Vec<DialogSection>,
        session: Entity<AgentSession>,
        workspace: WeakEntity<Workspace>,
        cx: &mut Context<Self>,
    ) -> Self {
        let reload: Reload = Box::new({
            let session = session.clone();
            move |cx| {
                let answer = session.update(cx, |session, cx| session.control(request.clone(), cx));
                cx.spawn(async move |_| Ok(sections(&answer.await?)))
            }
        });
        Self::new(title, session, workspace, reload, cx)
    }

    fn output_styles(
        session: Entity<AgentSession>,
        workspace: WeakEntity<Workspace>,
        cx: &mut Context<Self>,
    ) -> Self {
        let reload: Reload = Box::new({
            let session = session.clone();
            move |cx| {
                let (styles, settings, live_run) = session.update(cx, |session, cx| {
                    (
                        session.control(json!({ "subtype": "reload_output_styles" }), cx),
                        session.control(json!({ "subtype": "get_settings" }), cx),
                        session.has_live_run(),
                    )
                });
                cx.spawn(async move |_| {
                    Ok(output_style_sections(
                        &styles.await?,
                        &settings.await?,
                        live_run,
                    ))
                })
            }
        });
        Self::new(t!("Output Style"), session, workspace, reload, cx)
    }

    fn mcp(
        session: Entity<AgentSession>,
        workspace: WeakEntity<Workspace>,
        cx: &mut Context<Self>,
    ) -> Self {
        let reload: Reload = Box::new({
            let session = session.clone();
            move |cx| {
                let (status, live_run) = session.update(cx, |session, cx| {
                    (
                        session.control(json!({ "subtype": "mcp_status" }), cx),
                        session.has_live_run(),
                    )
                });
                cx.spawn(async move |_| {
                    let mut sections = mcp_sections(&status.await?);
                    if !live_run {
                        sections.insert(
                            0,
                            DialogSection {
                                title: t!("Note"),
                                rows: vec![DialogRow::new(t!(
                                    "Start a turn first: enabling, disabling and reconnecting servers applies to the running Claude session."
                                ))],
                            },
                        );
                    }
                    Ok(sections)
                })
            }
        });
        let mut dialog = Self::new(t!("MCP servers"), session, workspace, reload, cx);
        dialog.can_add_server = true;
        dialog
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        let sections = (self.reload)(cx);
        self.state = DialogState::Loading;
        self._pending = cx.spawn(async move |this, cx| {
            let sections = sections.await;
            this.update(cx, |this, cx| {
                this.state = match sections {
                    Ok(sections) => DialogState::Loaded(sections),
                    Err(error) => DialogState::Failed(error.to_string().into()),
                };
                cx.notify();
            })
            .log_err();
        });
        cx.notify();
    }

    fn settle<T: 'static>(
        &mut self,
        task: Task<anyhow::Result<T>>,
        on_success: impl FnOnce(&mut Self, T, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) {
        self.state = DialogState::Loading;
        self._pending = cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| match result {
                Ok(value) => on_success(this, value, cx),
                Err(error) => {
                    this.state = DialogState::Failed(error.to_string().into());
                    cx.notify();
                }
            })
            .log_err();
        });
        cx.notify();
    }

    fn run_control(&mut self, request: Value, cx: &mut Context<Self>) {
        let answer = self
            .session
            .update(cx, |session, cx| session.control(request, cx));
        self.settle(answer, |this, _, cx| this.reload(cx), cx);
    }

    fn run_shell(&mut self, arguments: Vec<String>, cx: &mut Context<Self>) {
        let finished = self
            .session
            .update(cx, |session, cx| session.run_cli(arguments, cx));
        self.settle(finished, |this, (), cx| this.reload(cx), cx);
    }

    fn authenticate(&mut self, request: Value, cx: &mut Context<Self>) {
        let answer = self
            .session
            .update(cx, |session, cx| session.control(request, cx));
        self.settle(
            answer,
            |this, response, cx| match text(&response, "error") {
                Some(error) => {
                    this.state = DialogState::Failed(error);
                    cx.notify();
                }
                None => {
                    if let Some(url) = authentication_url(&response) {
                        cx.open_url(&url);
                    }
                    this.reload(cx);
                }
            },
            cx,
        );
    }

    fn open_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let name = cx.new(|cx| Editor::single_line(window, cx));
        let target = cx.new(|cx| Editor::single_line(window, cx));
        window.focus(&name.focus_handle(cx), cx);
        self.form = Some(AddServerForm {
            name,
            target,
            transport: MCP_TRANSPORTS[0],
            scope: MCP_SCOPES[0],
            error: None,
        });
        cx.notify();
    }

    fn submit_form(&mut self, cx: &mut Context<Self>) {
        let Some(form) = &mut self.form else {
            return;
        };
        let name = form.name.read(cx).text(cx).trim().to_string();
        let target = form.target.read(cx).text(cx).trim().to_string();
        if let Some(error) = server_input_error(&name, &target) {
            form.error = Some(error);
            cx.notify();
            return;
        }
        let arguments = add_server_arguments(form.transport, form.scope, &name, &target);
        self.form = None;
        self.run_shell(arguments, cx);
    }

    fn open(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        // The dialog closes first: a modal that loses focus to the prompt or the new tab is dismissed anyway.
        cx.emit(DismissEvent);
        let workspace = self.workspace.clone();
        let create = (!path.exists()).then(|| {
            window.prompt(
                PromptLevel::Info,
                &t!("Create {path}?", path = path.display()),
                None,
                &[
                    PromptButton::new(t!("Create")),
                    PromptButton::cancel(t!("Cancel")),
                ],
                cx,
            )
        });
        window
            .spawn(cx, async move |cx| {
                if let Some(answer) = create
                    && answer.await.ok() != Some(0)
                {
                    return anyhow::Ok(());
                }
                let opened = match create_empty_file(&path, cx).await {
                    Ok(()) => open_path(&workspace, path.clone(), cx).await,
                    Err(error) => Err(error),
                };
                if let Err(error) = opened {
                    let message = t!(
                        "Couldn't open {path}: {error}",
                        path = path.display(),
                        error = error
                    )
                    .to_string();
                    workspace.update(cx, |workspace, cx| {
                        workspace.show_toast(
                            Toast::new(NotificationId::unique::<ClaudeDialog>(), message)
                                .autohide(),
                            cx,
                        )
                    })?;
                }
                Ok(())
            })
            .detach_and_log_err(cx);
    }

    fn render_action(
        &self,
        id: SharedString,
        action: &RowAction,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match action {
            RowAction::OpenPath(path) => {
                let path = path.clone();
                Button::new(id, t!("Open"))
                    .on_click(
                        cx.listener(move |this, _, window, cx| this.open(path.clone(), window, cx)),
                    )
                    .into_any_element()
            }
            RowAction::OpenUrl(url) => {
                let url = url.clone();
                Button::new(id, t!("Open"))
                    .on_click(move |_, _, cx| cx.open_url(&url))
                    .into_any_element()
            }
            RowAction::Control { label, request } => {
                let request = request.clone();
                Button::new(id, label.clone())
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.run_control(request.clone(), cx)),
                    )
                    .into_any_element()
            }
            RowAction::Toggle {
                on,
                request_on,
                request_off,
            } => {
                let dialog = cx.weak_entity();
                let (request_on, request_off) = (request_on.clone(), request_off.clone());
                Switch::new(id, ToggleState::from(*on))
                    .on_click(move |state, _, cx| {
                        let request = if state.selected() {
                            request_on.clone()
                        } else {
                            request_off.clone()
                        };
                        dialog
                            .update(cx, |dialog, cx| dialog.run_control(request, cx))
                            .log_err();
                    })
                    .into_any_element()
            }
            RowAction::Shell { label, arguments } => {
                let arguments = arguments.clone();
                Button::new(id, label.clone())
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.run_shell(arguments.clone(), cx)),
                    )
                    .into_any_element()
            }
            RowAction::Authenticate { label, request } => {
                let request = request.clone();
                Button::new(id, label.clone())
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.authenticate(request.clone(), cx)),
                    )
                    .into_any_element()
            }
        }
    }

    fn render_form(&self, form: &AddServerForm, cx: &mut Context<Self>) -> Section {
        let field = |label: SharedString, editor: &Entity<Editor>| {
            v_flex()
                .gap_1()
                .child(Label::new(label).size(LabelSize::Small).color(Color::Muted))
                .child(
                    div()
                        .w_full()
                        .px_2()
                        .py_1()
                        .rounded_sm()
                        .border_1()
                        .border_color(cx.theme().colors().border)
                        .child(editor.clone()),
                )
        };
        let choice = |label: SharedString,
                      name: &'static str,
                      options: [&'static str; 3],
                      current: &'static str,
                      select: fn(&mut AddServerForm, &'static str)| {
            let buttons = options.map(|option| {
                ToggleButtonSimple::new(
                    option,
                    cx.listener(move |this, _, _, cx| {
                        if let Some(form) = &mut this.form {
                            select(form, option);
                            cx.notify();
                        }
                    }),
                )
                .selected(option == current)
            });
            v_flex()
                .gap_1()
                .child(Label::new(label).size(LabelSize::Small).color(Color::Muted))
                .child(
                    ToggleButtonGroup::single_row(name, buttons).selected_index(
                        options
                            .iter()
                            .position(|option| *option == current)
                            .unwrap_or_default(),
                    ),
                )
        };
        Section::new().child(
            v_flex()
                .gap_2()
                .child(field(t!("Name"), &form.name))
                .child(choice(
                    t!("Transport"),
                    "claude-mcp-transport",
                    MCP_TRANSPORTS,
                    form.transport,
                    |form, value| form.transport = value,
                ))
                .child(field(t!("URL or command"), &form.target))
                .child(choice(
                    t!("Scope"),
                    "claude-mcp-scope",
                    MCP_SCOPES,
                    form.scope,
                    |form, value| form.scope = value,
                ))
                .when_some(form.error.clone(), |this, error| {
                    this.child(Label::new(error).color(Color::Error))
                }),
        )
    }

    fn render_row(&self, id: &str, row: &DialogRow, cx: &mut Context<Self>) -> impl IntoElement {
        let actions: Vec<AnyElement> = row
            .actions
            .iter()
            .enumerate()
            .map(|(index, action)| self.render_action(format!("{id}-{index}").into(), action, cx))
            .collect();
        h_flex()
            .w_full()
            .gap_2()
            .py_0p5()
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(
                        h_flex()
                            .gap_1p5()
                            .child(Label::new(row.label.clone()))
                            .when_some(row.badge.clone(), |this, badge| {
                                this.child(
                                    Label::new(badge).size(LabelSize::Small).color(Color::Muted),
                                )
                            }),
                    )
                    .when_some(row.detail.clone(), |this, detail| {
                        this.child(
                            Label::new(detail)
                                .size(LabelSize::Small)
                                .color(Color::Muted)
                                .truncate(),
                        )
                    }),
            )
            .children(actions)
    }
}

async fn create_empty_file(
    path: &std::path::Path,
    cx: &mut gpui::AsyncWindowContext,
) -> anyhow::Result<()> {
    if path.exists() {
        return Ok(());
    }
    let path = path.to_path_buf();
    cx.background_spawn(async move {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        match std::fs::File::create_new(&path) {
            Err(error) if error.kind() != std::io::ErrorKind::AlreadyExists => Err(error.into()),
            _ => Ok(()),
        }
    })
    .await
}

impl Render for ClaudeDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut sections = Vec::new();
        if let Some(form) = &self.form {
            sections.push(self.render_form(form, cx));
        } else {
            match &self.state {
                DialogState::Loading => sections
                    .push(Section::new().child(Label::new(t!("Loading…")).color(Color::Muted))),
                DialogState::Failed(message) => sections
                    .push(Section::new().child(Label::new(message.clone()).color(Color::Error))),
                DialogState::Loaded(loaded) if loaded.is_empty() => sections.push(
                    Section::new().child(Label::new(t!("Nothing to show")).color(Color::Muted)),
                ),
                DialogState::Loaded(loaded) => {
                    for (section_index, section) in loaded.iter().enumerate() {
                        let rows: Vec<AnyElement> = section
                            .rows
                            .iter()
                            .enumerate()
                            .map(|(row_index, row)| {
                                self.render_row(
                                    &format!("claude-dialog-{section_index}-{row_index}"),
                                    row,
                                    cx,
                                )
                                .into_any_element()
                            })
                            .collect();
                        sections.push(
                            Section::new()
                                .header(SectionHeader::new(section.title.clone()))
                                .children(rows),
                        );
                    }
                }
            }
        }
        let failed = matches!(self.state, DialogState::Failed(_));
        let can_add = self.can_add_server && matches!(self.state, DialogState::Loaded(_));
        let mut modal = Modal::new("claude-dialog", Some(self.scroll_handle.clone())).header(
            ModalHeader::new()
                .show_dismiss_button(true)
                .headline(self.title.clone()),
        );
        for section in sections {
            modal = modal.section(section);
        }
        if self.form.is_some() {
            modal = modal.footer(
                ModalFooter::new().end_slot(
                    h_flex()
                        .gap_1()
                        .child(
                            Button::new("claude-dialog-cancel-add", t!("Cancel")).on_click(
                                cx.listener(|this, _, _, cx| {
                                    this.form = None;
                                    cx.notify();
                                }),
                            ),
                        )
                        .child(
                            Button::new("claude-dialog-submit-add", t!("Add server"))
                                .style(ButtonStyle::Filled)
                                .on_click(cx.listener(|this, _, _, cx| this.submit_form(cx))),
                        ),
                ),
            );
        } else if failed || can_add {
            modal = modal.footer(
                ModalFooter::new()
                    .start_slot::<Button>(can_add.then(|| {
                        Button::new("claude-dialog-add-server", t!("Add server"))
                            .on_click(cx.listener(|this, _, window, cx| this.open_form(window, cx)))
                    }))
                    .end_slot::<Button>(failed.then(|| {
                        Button::new("claude-dialog-close", t!("Close"))
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(DismissEvent)))
                    })),
            );
        }
        v_flex()
            .track_focus(&self.focus_handle)
            .elevation_3(cx)
            .on_action(cx.listener(|_, _: &menu::Cancel, _, cx| cx.emit(DismissEvent)))
            .occlude()
            .w(rems(34.))
            .max_h(rems(40.))
            .child(modal)
    }
}

impl ChatView {
    pub(super) fn show_claude_dialog(
        &mut self,
        command: &AppCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        type Build = Box<
            dyn FnOnce(
                Entity<AgentSession>,
                WeakEntity<Workspace>,
                &mut Context<ClaudeDialog>,
            ) -> ClaudeDialog,
        >;
        let control = |title: SharedString,
                       subtype: &'static str,
                       sections: fn(&Value) -> Vec<DialogSection>|
         -> Build {
            Box::new(move |session, workspace, cx| {
                ClaudeDialog::for_control(
                    title,
                    json!({ "subtype": subtype }),
                    sections,
                    session,
                    workspace,
                    cx,
                )
            })
        };
        let build = match command {
            AppCommand::Status => control(t!("Status"), "get_status", status_sections),
            AppCommand::Hooks => control(t!("Hooks"), "get_hooks_listing", hooks_sections),
            AppCommand::Memory => control(t!("Memory"), "get_memory_dialog", memory_sections),
            AppCommand::Skills => control(t!("Skills"), "get_skills_dialog", skills_sections),
            AppCommand::Sandbox => control(t!("Sandbox"), "get_sandbox_dialog", sandbox_sections),
            AppCommand::OutputStyle => Box::new(ClaudeDialog::output_styles),
            AppCommand::Mcp => Box::new(ClaudeDialog::mcp),
            AppCommand::Model
            | AppCommand::New
            | AppCommand::Resume
            | AppCommand::Settings
            | AppCommand::Diff
            | AppCommand::Files
            | AppCommand::Terminal
            | AppCommand::Stop
            | AppCommand::Plan { .. }
            | AppCommand::Fork { .. }
            | AppCommand::Rewind => return,
        };
        let session = self.session.clone();
        let weak_workspace = self.workspace.clone();
        self.workspace
            .update(cx, |workspace, cx| {
                workspace.toggle_modal(window, cx, move |_, cx| build(session, weak_workspace, cx))
            })
            .log_err();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        AgentKind,
        session::tests::{new_store, run_until},
    };
    use gpui::TestAppContext;

    fn paths(row: &DialogRow) -> Vec<PathBuf> {
        row.actions
            .iter()
            .filter_map(|action| match action {
                RowAction::OpenPath(path) => Some(path.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn status_sections_keep_order() {
        let response = json!({
            "sections": [
                {
                    "title": "Session",
                    "rows": [
                        { "label": "Version", "value": "2.1.332" },
                        { "value": "Managed by your IT administrator." }
                    ]
                },
                { "title": "Environment", "rows": [{ "label": "Model", "value": "claude-opus-5-5" }] }
            ]
        });
        let sections = status_sections(&response);
        let titles: Vec<&str> = sections
            .iter()
            .map(|section| section.title.as_ref())
            .collect();
        assert_eq!(titles, ["Session", "Environment"]);
        assert_eq!(sections[0].rows[0].label, "Version");
        assert_eq!(sections[0].rows[0].detail.as_deref(), Some("2.1.332"));
        assert_eq!(
            sections[0].rows[1].label,
            "Managed by your IT administrator."
        );
        assert_eq!(sections[0].rows[1].detail, None);
        assert_eq!(
            sections[1].rows[0].detail.as_deref(),
            Some("claude-opus-5-5")
        );
    }

    #[test]
    fn hooks_sections_group_by_event() {
        let response = json!({
            "hooks": [
                { "event": "PreToolUse", "matcher": "Bash", "source": "userSettings" },
                { "event": "Stop", "matcher": "", "source": "projectSettings" },
                { "event": "PreToolUse", "matcher": "Edit", "source": "pluginHook", "pluginName": "lint@market" }
            ],
            "events": [
                { "name": "PreToolUse", "summary": "Before tool execution" },
                { "name": "Stop", "summary": "Right before Claude concludes its response" }
            ]
        });
        let sections = hooks_sections(&response);
        let titles: Vec<&str> = sections
            .iter()
            .map(|section| section.title.as_ref())
            .collect();
        assert_eq!(
            titles,
            [
                "PreToolUse · Before tool execution",
                "Stop · Right before Claude concludes its response"
            ]
        );
        let labels: Vec<&str> = sections[0]
            .rows
            .iter()
            .map(|row| row.label.as_ref())
            .collect();
        assert_eq!(labels, ["Bash", "Edit"]);
        assert_eq!(sections[0].rows[0].badge.as_deref(), Some("userSettings"));
        assert_eq!(sections[0].rows[1].badge.as_deref(), Some("lint@market"));
        assert_eq!(sections[1].rows[0].label, "*");
    }

    #[test]
    fn memory_sections_list_files_with_open_actions() {
        let mut response = json!({
            "files": [
                { "kind": "user", "path": "/home/me/.claude/CLAUDE.md", "label": "User instructions", "description": "Saved in ~/.claude/CLAUDE.md", "exists": true },
                { "kind": "project", "path": "/repo/CLAUDE.md", "label": "Project instructions", "description": "Checked in at ./CLAUDE.md", "exists": false }
            ],
            "memories": [
                { "name": "MEMORY.md", "path": "/home/me/memory/MEMORY.md", "description": "Index of saved memories" }
            ],
            "auto_memory": { "enabled": true, "toggleable": true, "status": "on" }
        });
        let sections = memory_sections(&response);
        let files = &sections[0].rows;
        assert_eq!(files[0].label, "User instructions");
        assert_eq!(
            paths(&files[0]),
            [PathBuf::from("/home/me/.claude/CLAUDE.md")]
        );
        assert_eq!(files[0].badge, None);
        assert_eq!(paths(&files[1]), [PathBuf::from("/repo/CLAUDE.md")]);
        assert_eq!(files[1].badge, Some(t!("Not created yet")));
        let auto = &sections[1].rows;
        assert_eq!(
            auto[0].actions,
            [RowAction::Toggle {
                on: true,
                request_on: json!({ "subtype": "update_settings", "source": "userSettings", "settings": { "autoMemoryEnabled": true } }),
                request_off: json!({ "subtype": "update_settings", "source": "userSettings", "settings": { "autoMemoryEnabled": false } }),
            }]
        );
        assert_eq!(
            paths(&auto[1]),
            [PathBuf::from("/home/me/memory/MEMORY.md")]
        );

        response["auto_memory"]["toggleable"] = json!(false);
        assert_eq!(memory_sections(&response)[1].rows[0].actions, []);
        assert_eq!(memory_sections(&json!({})), []);
    }

    #[test]
    fn skills_sections_group_by_source() {
        let response = json!({
            "skills": [
                { "name": "review", "display_name": "review", "state": "on", "source": "userSettings", "tokens": 120 },
                { "name": "lint:check", "display_name": "check", "state": "off", "source": "plugin", "tokens": 80 },
                { "name": "notes", "display_name": "notes", "state": "on", "source": "userSettings", "tokens": 40 }
            ]
        });
        let sections = skills_sections(&response);
        let titles: Vec<&str> = sections
            .iter()
            .map(|section| section.title.as_ref())
            .collect();
        assert_eq!(titles, ["userSettings", "plugin"]);
        let labels: Vec<&str> = sections[0]
            .rows
            .iter()
            .map(|row| row.label.as_ref())
            .collect();
        assert_eq!(labels, ["review", "notes"]);
        assert_eq!(sections[1].rows[0].label, "check");
        assert_eq!(sections[1].rows[0].badge.as_deref(), Some("off"));
    }

    #[test]
    fn sandbox_sections_show_state_and_non_empty_rules() {
        let response = json!({
            "supported": false,
            "unsupported_reason": "platform",
            "enabled": false,
            "mode": "disabled",
            "excluded_commands": ["docker"],
            "restrictions": { "fs_allow_write": ["/tmp", "/var/tmp"], "network_allowed_domains": [] }
        });
        let sections = sandbox_sections(&response);
        let state: Vec<(&str, Option<&str>)> = sections[0]
            .rows
            .iter()
            .map(|row| (row.label.as_ref(), row.detail.as_deref()))
            .collect();
        assert_eq!(
            state,
            [
                ("Supported", Some("No (platform)")),
                ("Enabled", Some("No")),
                ("Mode", Some("disabled"))
            ]
        );
        assert_eq!(sections[1].rows[0].label, "docker");
        assert_eq!(sections[2].rows.len(), 1);
        assert_eq!(sections[2].rows[0].label, "fs_allow_write");
        assert_eq!(
            sections[2].rows[0].detail.as_deref(),
            Some("/tmp, /var/tmp")
        );
    }

    #[test]
    fn output_styles_mark_the_current_one() {
        let styles = json!({ "available_output_styles": ["default", "Concise"] });
        let rows = &output_style_sections(
            &styles,
            &json!({ "effective": { "outputStyle": "Concise" } }),
            true,
        )[0]
        .rows;
        assert_eq!(
            rows[0].actions,
            [RowAction::Control {
                label: t!("Use"),
                request: json!({ "subtype": "apply_flag_settings", "settings": { "outputStyle": "default" } }),
            }]
        );
        assert_eq!(rows[1].badge, Some(t!("Current")));
        assert_eq!(rows[1].actions, []);

        let rows = &output_style_sections(&styles, &json!({ "effective": {} }), true)[0].rows;
        assert_eq!(rows[0].badge, Some(t!("Current")));
    }

    #[test]
    fn output_styles_without_a_live_run_explain_instead_of_offering_use() {
        let styles = json!({ "available_output_styles": ["default", "Concise"] });
        let rows = &output_style_sections(&styles, &json!({ "effective": {} }), false)[0].rows;
        assert_eq!(
            rows[0].label,
            t!("Start a turn first: the output style applies to the running Claude session.")
        );
        let labels: Vec<&str> = rows[1..].iter().map(|row| row.label.as_ref()).collect();
        assert_eq!(labels, ["default", "Concise"]);
        assert!(rows.iter().all(|row| row.actions.is_empty()));
    }

    fn action_labels(row: &DialogRow) -> Vec<SharedString> {
        row.actions
            .iter()
            .map(|action| match action {
                RowAction::Control { label, .. }
                | RowAction::Authenticate { label, .. }
                | RowAction::Shell { label, .. } => label.clone(),
                other => panic!("unexpected action {other:?}"),
            })
            .collect()
    }

    fn mcp_row(response: &Value, name: &str) -> DialogRow {
        mcp_sections(response)
            .into_iter()
            .flat_map(|section| section.rows)
            .find(|row| row.label == name)
            .expect("the server is listed")
    }

    #[test]
    fn mcp_rows_offer_actions_by_status() {
        let response = json!({
            "mcpServers": [
                { "name": "files", "status": "connected", "scope": "user", "config": { "type": "stdio" } },
                { "name": "github", "status": "connected", "scope": "project", "config": { "type": "http" } },
                { "name": "off", "status": "disabled", "scope": "local" },
                { "name": "broken", "status": "failed", "scope": "user", "error": "spawn ENOENT" },
                { "name": "linear", "status": "needs-auth", "scope": "user", "config": { "type": "http" } },
                { "name": "booting", "status": "pending" },
                { "name": "plugin", "status": "connected", "scope": "dynamic", "config": { "type": "http" } },
                { "name": "proxy", "status": "connected", "scope": "user", "config": { "type": "claudeai-proxy" } }
            ]
        });
        let labels = |name: &str| action_labels(&mcp_row(&response, name));
        assert_eq!(
            labels("files"),
            [t!("Disable"), t!("Reconnect"), t!("Remove")]
        );
        assert_eq!(
            labels("github"),
            [
                t!("Disable"),
                t!("Reconnect"),
                t!("Clear authentication"),
                t!("Remove")
            ]
        );
        assert_eq!(labels("off"), [t!("Enable"), t!("Remove")]);
        assert_eq!(labels("broken"), [t!("Reconnect"), t!("Remove")]);
        assert_eq!(
            mcp_row(&response, "broken").detail.as_deref(),
            Some("spawn ENOENT")
        );
        assert_eq!(labels("linear"), [t!("Authenticate"), t!("Remove")]);
        assert_eq!(labels("booting"), Vec::<SharedString>::new());
        assert_eq!(
            labels("plugin"),
            [t!("Disable"), t!("Reconnect"), t!("Clear authentication")]
        );
        assert_eq!(
            labels("proxy"),
            [t!("Disable"), t!("Reconnect"), t!("Remove")]
        );

        let github = mcp_row(&response, "github");
        assert_eq!(
            github.actions[0],
            RowAction::Control {
                label: t!("Disable"),
                request: json!({ "subtype": "mcp_toggle", "serverName": "github", "enabled": false }),
            }
        );
        assert_eq!(
            mcp_row(&response, "off").actions[0],
            RowAction::Control {
                label: t!("Enable"),
                request: json!({ "subtype": "mcp_toggle", "serverName": "off", "enabled": true }),
            }
        );
        assert_eq!(
            mcp_row(&response, "linear").actions[0],
            RowAction::Authenticate {
                label: t!("Authenticate"),
                request: json!({ "subtype": "mcp_authenticate", "serverName": "linear" }),
            }
        );
        assert_eq!(
            github.actions[3],
            RowAction::Shell {
                label: t!("Remove"),
                arguments: ["mcp", "remove", "--scope", "project", "github"]
                    .map(String::from)
                    .into(),
            }
        );
    }

    #[test]
    fn add_server_builds_the_cli_arguments() {
        assert_eq!(
            add_server_arguments("http", "user", "github", "https://example.com/mcp"),
            [
                "mcp",
                "add",
                "--transport",
                "http",
                "--scope",
                "user",
                "github",
                "https://example.com/mcp"
            ]
        );
    }

    #[test]
    fn server_names_are_passed_as_single_arguments() {
        let name = "my server; rm -rf /";
        let add = add_server_arguments("stdio", "local", name, "npx");
        assert_eq!(add.len(), 8);
        assert_eq!(add[6], name);
        let response =
            json!({ "mcpServers": [{ "name": name, "status": "disabled", "scope": "local" }] });
        let Some(RowAction::Shell { arguments, .. }) = mcp_row(&response, name).actions.pop()
        else {
            panic!("the row offers no removal");
        };
        assert_eq!(arguments, ["mcp", "remove", "--scope", "local", name]);
    }

    #[test]
    fn add_server_input_is_validated() {
        assert!(server_input_error("github", "https://example.com/mcp").is_none());
        assert!(server_input_error("", "https://example.com/mcp").is_some());
        assert!(server_input_error("github", "").is_some());
        assert!(server_input_error("--scope", "https://example.com/mcp").is_some());
        assert!(server_input_error("github", "--help").is_some());
    }

    #[test]
    fn only_web_addresses_are_opened_for_authentication() {
        let url = |value: Value| authentication_url(&json!({ "authUrl": value }));
        assert_eq!(
            url(json!("https://auth.example.com/start")).as_deref(),
            Some("https://auth.example.com/start")
        );
        assert_eq!(url(json!("file:///etc/passwd")), None);
        assert_eq!(url(json!("javascript:alert(1)")), None);
        assert_eq!(url(json!(null)), None);
    }

    #[gpui::test]
    async fn control_error_is_shown_not_hung(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = new_store(directory.path(), cx);
        let session = store.update(cx, |store, cx| {
            store.create_session(AgentKind::Claude, directory.path().to_path_buf(), cx)
        });
        let dialog = cx.new(|cx| {
            ClaudeDialog::for_control(
                "Plan".into(),
                json!({ "subtype": "get_plan" }),
                status_sections,
                session,
                WeakEntity::new_invalid(),
                cx,
            )
        });
        let settled = |cx: &mut TestAppContext| {
            dialog.read_with(cx, |dialog, _| {
                !matches!(dialog.state, DialogState::Loading)
            })
        };
        run_until(cx, &settled);
        dialog.update(cx, |dialog, cx| {
            assert!(matches!(dialog.state, DialogState::Loaded(_)));
            assert!(!dialog.session.read(cx).has_live_run());
            dialog.run_control(json!({ "subtype": "mcp_status" }), cx);
        });
        run_until(cx, &settled);
        dialog.read_with(cx, |dialog, _| match &dialog.state {
            DialogState::Failed(message) => {
                assert!(message.contains("MCP servers are unavailable"), "{message}")
            }
            _ => panic!("the CLI's error isn't shown"),
        });
    }
}
