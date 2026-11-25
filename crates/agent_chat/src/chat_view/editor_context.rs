use super::ChatView;
use crate::{
    AgentChatSettings, AgentKind,
    chat_style::{icon, ink},
};
use editor::{Editor, EditorEvent, ToPoint as _};
use gpui::{
    AnyElement, App, Context, Entity, IntoElement, ParentElement, SharedString, Styled,
    Subscription, div, px,
};
use i18n::t;
use language::File as _;
use settings::Settings as _;
use std::path::PathBuf;
use ui::{IconName, Tooltip, prelude::*};
use workspace::Workspace;

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct EditorContext {
    pub absolute_path: PathBuf,
    pub selection: Option<SelectedLines>,
    pub path_only: bool,
}

/// Lines are 1-based and inclusive.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SelectedLines {
    pub start: u32,
    pub end: u32,
    pub text: String,
}

#[derive(Default)]
pub(super) struct EditorContextState {
    current: Option<EditorContext>,
    sent: Option<EditorContext>,
    dismissed: Option<EditorContext>,
    hidden: bool,
    _active_editor: Option<Subscription>,
}

impl EditorContextState {
    /// The context the footer chip shows: neither sent already nor dismissed.
    fn shown(&self) -> Option<&EditorContext> {
        self.current
            .as_ref()
            .filter(|context| Some(*context) != self.sent.as_ref())
            .filter(|context| Some(*context) != self.dismissed.as_ref())
    }
}

fn context_editor(workspace: &Workspace, cx: &App) -> Option<Entity<Editor>> {
    let active = workspace.active_item(cx)?;
    if active.downcast::<ChatView>().is_none() {
        return active.act_as::<Editor>(cx);
    }
    // A chat open as a tab is the active item while you type in it, so use the editor you were in before. Only plain editors count there: resolving a wrapped one would read the chat being updated.
    workspace
        .panes()
        .iter()
        .flat_map(|pane| {
            let pane = pane.read(cx);
            pane.activation_history().iter().filter_map(|entry| {
                let item = pane
                    .items()
                    .find(|item| item.item_id() == entry.entity_id)?;
                Some((entry.timestamp, item.downcast::<Editor>()?))
            })
        })
        .max_by_key(|(timestamp, _)| *timestamp)
        .map(|(_, editor)| editor)
}

pub(crate) fn current_editor_context(workspace: &Workspace, cx: &App) -> Option<EditorContext> {
    let settings = AgentChatSettings::get_global(cx);
    let editor = context_editor(workspace, cx)?;
    let editor = editor.read(cx);
    let buffer = editor.active_buffer(cx)?;
    let file = project::File::from_dyn(buffer.read(cx).file())?;
    let absolute_path = file.as_local()?.abs_path(cx);
    // Excluded files have no worktree entry.
    let path_only = file
        .worktree
        .read(cx)
        .entry_for_path(&file.path)
        .is_none_or(|entry| entry.is_ignored);
    let selection = (settings.attach_selection && !path_only)
        .then(|| selected_lines(editor, cx))
        .flatten();
    if selection.is_none() && !settings.attach_open_file {
        return None;
    }
    Some(EditorContext {
        absolute_path,
        selection,
        path_only,
    })
}

fn selected_lines(editor: &Editor, cx: &App) -> Option<SelectedLines> {
    let snapshot = editor.buffer().read(cx).snapshot(cx);
    let range = editor.selections.newest_anchor().range();
    let range = range.start.to_point(&snapshot)..range.end.to_point(&snapshot);
    if range.is_empty() {
        return None;
    }
    let (buffer, range) = snapshot.range_to_buffer_range(range)?;
    let text: String = buffer.text_for_range(range.clone()).collect();
    // A selection of whole lines ends at the start of the next line, which isn't selected.
    let end = if range.end.column == 0 && range.end.row > range.start.row {
        range.end.row
    } else {
        range.end.row + 1
    };
    Some(SelectedLines {
        start: range.start.row + 1,
        end,
        text,
    })
}

fn selection_shown(context: &EditorContext) -> Option<&SelectedLines> {
    context.selection.as_ref().filter(|_| !context.path_only)
}

pub(crate) fn context_block(context: &EditorContext) -> String {
    let path = context.absolute_path.display();
    match selection_shown(context) {
        Some(lines) => format!(
            "<ide_selection>The user selected the lines {} to {} from {path}:\n{}\n\nThis may or may not be related to the current task.</ide_selection>",
            lines.start, lines.end, lines.text
        ),
        None => format!(
            "<ide_opened_file>The user opened the file {path} in the IDE. This may or may not be related to the current task.</ide_opened_file>"
        ),
    }
}

impl ChatView {
    pub(super) fn watch_active_editor(&mut self, cx: &mut Context<Self>) {
        let editor = self
            .workspace
            .upgrade()
            .and_then(|workspace| context_editor(workspace.read(cx), cx));
        self.editor_context._active_editor = editor.map(|editor| {
            cx.subscribe(&editor, |this, _, event: &EditorEvent, cx| {
                if matches!(event, EditorEvent::SelectionsChanged { .. }) {
                    this.refresh_editor_context(cx);
                }
            })
        });
        self.refresh_editor_context(cx);
    }

    pub(super) fn refresh_editor_context(&mut self, cx: &mut Context<Self>) {
        let current = (self.subagent.is_none()
            && self.session.read(cx).kind() == AgentKind::Claude)
            .then(|| self.workspace.upgrade())
            .flatten()
            .and_then(|workspace| current_editor_context(workspace.read(cx), cx));
        if self.editor_context.current != current {
            self.editor_context.current = current;
            cx.notify();
        }
    }

    /// The context blocks for a message about to be sent; each context goes out once.
    pub(super) fn take_editor_context(
        &mut self,
        text: &str,
        cx: &mut Context<Self>,
    ) -> Vec<String> {
        if self.editing_queued.is_some() || agent_harness::leading_command(text).is_some() {
            return Vec::new();
        }
        self.refresh_editor_context(cx);
        let state = &mut self.editor_context;
        if state.hidden {
            return Vec::new();
        }
        let Some(context) = state.shown().cloned() else {
            return Vec::new();
        };
        let block = context_block(&context);
        state.sent = Some(context);
        cx.notify();
        vec![block]
    }

    /// The agent forgets an edited message, so its context goes out again.
    pub(super) fn forget_sent_editor_context(&mut self) {
        self.editor_context.sent = None;
    }

    pub(super) fn dismiss_editor_context(&mut self, cx: &mut Context<Self>) {
        self.editor_context.dismissed = self.editor_context.current.clone();
        cx.notify();
    }

    pub(super) fn render_editor_context_chip(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let context = self.editor_context.shown()?;
        let hidden = self.editor_context.hidden;
        let color = cx.theme().colors().text_muted.opacity(0.6);
        let hover = ink(0.07, cx);
        let label: SharedString = match selection_shown(context) {
            Some(lines) if lines.start == lines.end => t!("1 line selected"),
            Some(lines) => t!(
                "{count} lines selected",
                count = lines.end - lines.start + 1
            ),
            None => context
                .absolute_path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned().into())
                .unwrap_or_default(),
        };
        let button = |id: &'static str, icon_name: IconName| {
            div()
                .id(id)
                .size(px(16.))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(4.))
                .cursor_pointer()
                .hover(move |style| style.bg(hover))
                .child(icon(icon_name, px(12.), color))
        };
        Some(
            h_flex()
                .h(px(20.))
                .min_w_0()
                .max_w(px(280.))
                .px(px(8.))
                .gap(px(4.))
                .text_size(crate::chat_style::ui(12.))
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(color)
                .child(
                    button(
                        "agent-editor-context-toggle",
                        if hidden {
                            IconName::EyeOff
                        } else {
                            IconName::Eye
                        },
                    )
                    .tooltip(Tooltip::text(if hidden {
                        t!("Send with the next message")
                    } else {
                        t!("Don't send with the next message")
                    }))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.editor_context.hidden = !this.editor_context.hidden;
                        cx.notify();
                    })),
                )
                .child(
                    div()
                        .id("agent-editor-context-label")
                        .min_w_0()
                        .truncate()
                        .when(hidden, |this| this.line_through())
                        .tooltip(Tooltip::text(context.absolute_path.display().to_string()))
                        .child(label),
                )
                .child(
                    button("agent-editor-context-dismiss", IconName::Close)
                        .tooltip(Tooltip::text(t!("Dismiss")))
                        .on_click(cx.listener(|this, _, _, cx| this.dismiss_editor_context(cx))),
                )
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_block_matches_the_ide_format() {
        let context = EditorContext {
            absolute_path: PathBuf::from("/repo/src/main.rs"),
            selection: Some(SelectedLines {
                start: 3,
                end: 5,
                text: "let a = 1;\nlet b = 2;\nlet c = 3;".into(),
            }),
            path_only: false,
        };
        assert_eq!(
            context_block(&context),
            "<ide_selection>The user selected the lines 3 to 5 from /repo/src/main.rs:\nlet a = 1;\nlet b = 2;\nlet c = 3;\n\nThis may or may not be related to the current task.</ide_selection>"
        );
    }

    #[test]
    fn opened_file_block_matches_the_ide_format() {
        let expected = "<ide_opened_file>The user opened the file /repo/.env in the IDE. This may or may not be related to the current task.</ide_opened_file>";
        let mut context = EditorContext {
            absolute_path: PathBuf::from("/repo/.env"),
            selection: None,
            path_only: false,
        };
        assert_eq!(context_block(&context), expected);
        context.selection = Some(SelectedLines {
            start: 1,
            end: 1,
            text: "SECRET=1".into(),
        });
        context.path_only = true;
        assert_eq!(context_block(&context), expected);
    }
}
