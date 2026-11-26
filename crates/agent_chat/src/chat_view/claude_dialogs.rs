use super::{ChatView, composer::open_path};
use crate::{session::AgentSession, slash_commands::AppCommand};
use gpui::{
    App, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable, PromptButton,
    PromptLevel, Render, ScrollHandle, SharedString, Task, WeakEntity, Window, rems,
};
use i18n::t;
use serde_json::{Value, json};
use std::path::PathBuf;
use ui::{
    Modal, ModalFooter, ModalHeader, Section, SectionHeader, Switch, ToggleState, prelude::*,
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
    reason = "OpenUrl and Shell rows come with the dialogs that need them"
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

    fn run_control(&mut self, request: Value, cx: &mut Context<Self>) {
        let answer = self
            .session
            .update(cx, |session, cx| session.control(request, cx));
        self.state = DialogState::Loading;
        self._pending = cx.spawn(async move |this, cx| {
            let answer = answer.await;
            this.update(cx, |this, cx| match answer {
                Ok(_) => this.reload(cx),
                Err(error) => {
                    this.state = DialogState::Failed(error.to_string().into());
                    cx.notify();
                }
            })
            .log_err();
        });
        cx.notify();
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
    ) -> Option<AnyElement> {
        let element = match action {
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
            // shortcut: Shell rows are not shown until a dialog needs one, then run the CLI with these arguments (never through a shell).
            RowAction::Shell { .. } => return None,
        };
        Some(element)
    }

    fn render_row(&self, id: &str, row: &DialogRow, cx: &mut Context<Self>) -> impl IntoElement {
        let actions: Vec<AnyElement> = row
            .actions
            .iter()
            .enumerate()
            .filter_map(|(index, action)| {
                self.render_action(format!("{id}-{index}").into(), action, cx)
            })
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
        match &self.state {
            DialogState::Loading => {
                sections.push(Section::new().child(Label::new(t!("Loading…")).color(Color::Muted)))
            }
            DialogState::Failed(message) => {
                sections.push(Section::new().child(Label::new(message.clone()).color(Color::Error)))
            }
            DialogState::Loaded(loaded) if loaded.is_empty() => sections
                .push(Section::new().child(Label::new(t!("Nothing to show")).color(Color::Muted))),
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
        let failed = matches!(self.state, DialogState::Failed(_));
        let mut modal = Modal::new("claude-dialog", Some(self.scroll_handle.clone())).header(
            ModalHeader::new()
                .show_dismiss_button(true)
                .headline(self.title.clone()),
        );
        for section in sections {
            modal = modal.section(section);
        }
        if failed {
            modal = modal.footer(
                ModalFooter::new().end_slot(
                    Button::new("claude-dialog-close", t!("Close"))
                        .on_click(cx.listener(|_, _, _, cx| cx.emit(DismissEvent))),
                ),
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
