use super::ChatView;
use crate::{
    AgentChatSettings, AgentKind,
    chat_style::{icon, ink},
};
use editor::{Anchor, Editor, EditorEvent, ToPoint as _};
use gpui::{
    AnyElement, App, Context, Entity, EntityId, IntoElement, ParentElement, SharedString, Styled,
    Subscription, WeakEntity, div, px,
};
use i18n::t;
use language::File as _;
use settings::Settings as _;
use std::{
    ops::Range,
    path::{Path, PathBuf},
};
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

/// What the footer chip shows. The selected text is read only when sending, so this stays cheap to rebuild on every selection change.
#[derive(Clone, Debug, PartialEq)]
struct ShownContext {
    editor: WeakEntity<Editor>,
    absolute_path: PathBuf,
    /// The selection and its 1-based inclusive lines.
    selection: Option<(Range<Anchor>, u32, u32)>,
    path_only: bool,
}

impl ShownContext {
    /// Dismissing and sending key on which selection this is: edits change its text and line numbers but not its anchors.
    fn identity(&self) -> (EntityId, &Path, Option<&Range<Anchor>>) {
        (
            self.editor.entity_id(),
            &self.absolute_path,
            self.selection.as_ref().map(|(range, _, _)| range),
        )
    }

    fn full(&self, cx: &App) -> Option<EditorContext> {
        let selection = match &self.selection {
            Some((range, start, end)) => {
                let editor = self.editor.upgrade()?;
                let snapshot = editor.read(cx).buffer().read(cx).snapshot(cx);
                Some(SelectedLines {
                    start: *start,
                    end: *end,
                    text: snapshot.text_for_range(range.clone()).collect(),
                })
            }
            None => None,
        };
        Some(EditorContext {
            absolute_path: self.absolute_path.clone(),
            selection,
            path_only: self.path_only,
        })
    }
}

#[derive(Default)]
pub(super) struct EditorContextState {
    current: Option<ShownContext>,
    sent: Option<ShownContext>,
    dismissed: Option<ShownContext>,
    hidden: bool,
    _active_editor: Option<Subscription>,
}

impl EditorContextState {
    /// The context the footer chip shows: neither sent already nor dismissed.
    fn shown(&self) -> Option<&ShownContext> {
        let same = |context: &ShownContext, other: Option<&ShownContext>| {
            other.is_some_and(|other| other.identity() == context.identity())
        };
        self.current
            .as_ref()
            .filter(|context| !same(context, self.sent.as_ref()))
            .filter(|context| !same(context, self.dismissed.as_ref()))
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

fn shown_context(workspace: &Workspace, cx: &App) -> Option<ShownContext> {
    let settings = AgentChatSettings::get_global(cx);
    let editor_entity = context_editor(workspace, cx)?;
    let editor = editor_entity.read(cx);
    let buffer = editor.active_buffer(cx)?;
    let file = project::File::from_dyn(buffer.read(cx).file())?;
    let absolute_path = file.as_local()?.abs_path(cx);
    // Excluded files have no worktree entry; private ones are the `private_files` setting's matches, such as `.env`.
    let path_only = file
        .worktree
        .read(cx)
        .entry_for_path(&file.path)
        .is_none_or(|entry| entry.is_ignored || entry.is_private);
    let selection = (settings.attach_selection && !path_only)
        .then(|| selected_lines(editor, cx))
        .flatten();
    if selection.is_none() && !settings.attach_open_file {
        return None;
    }
    Some(ShownContext {
        editor: editor_entity.downgrade(),
        absolute_path,
        selection,
        path_only,
    })
}

/// The file and 1-based inclusive lines Alt+K mentions: the active editor's, or the one before it when the composer has focus.
pub(crate) fn mention_target(
    workspace: &Workspace,
    cx: &App,
) -> Option<(PathBuf, Option<(u32, u32)>)> {
    let editor_entity = context_editor(workspace, cx)?;
    let editor = editor_entity.read(cx);
    let buffer = editor.active_buffer(cx)?;
    let file = project::File::from_dyn(buffer.read(cx).file())?;
    let absolute_path = file.as_local()?.abs_path(cx);
    let lines = selected_lines(editor, cx).map(|(_, start, end)| (start, end));
    Some((absolute_path, lines))
}

pub(crate) fn line_suffix((start, end): (u32, u32)) -> String {
    if start == end {
        format!("#{start}")
    } else {
        format!("#{start}-{end}")
    }
}

/// Quoting and the line suffix follow Claude Code: a path with whitespace or `:` is quoted with the suffix inside, and one containing `"` or `#` gets no suffix since the suffix could not be told apart.
pub(crate) fn mention_text_with_lines(relative_path: &str, lines: Option<(u32, u32)>) -> String {
    let suffix = lines
        .filter(|_| !relative_path.contains(['"', '#']))
        .map(line_suffix)
        .unwrap_or_default();
    if relative_path.contains(char::is_whitespace) || relative_path.contains(':') {
        format!("@\"{relative_path}{suffix}\"")
    } else {
        format!("@{relative_path}{suffix}")
    }
}

fn selected_lines(editor: &Editor, cx: &App) -> Option<(Range<Anchor>, u32, u32)> {
    let snapshot = editor.buffer().read(cx).snapshot(cx);
    let anchors = editor.selections.newest_anchor().range();
    let range = anchors.start.to_point(&snapshot)..anchors.end.to_point(&snapshot);
    if range.is_empty() {
        return None;
    }
    // A selection spanning several files has no single location.
    let (_, range) = snapshot.range_to_buffer_range(range)?;
    // A selection of whole lines ends at the start of the next line, which isn't selected.
    let end = if range.end.column == 0 && range.end.row > range.start.row {
        range.end.row
    } else {
        range.end.row + 1
    };
    Some((anchors, range.start.row + 1, end))
}

pub(crate) fn context_block(context: &EditorContext) -> String {
    let path = context.absolute_path.display();
    match context.selection.as_ref().filter(|_| !context.path_only) {
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
                if matches!(
                    event,
                    EditorEvent::SelectionsChanged { .. } | EditorEvent::BufferEdited
                ) {
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
            .and_then(|workspace| shown_context(workspace.read(cx), cx));
        if self.editor_context.current != current {
            self.editor_context.current = current;
            cx.notify();
        }
    }

    /// The context blocks for a message about to be sent: only what the chip shows, and each context once.
    pub(super) fn take_editor_context(
        &mut self,
        text: &str,
        cx: &mut Context<Self>,
    ) -> Vec<String> {
        if self.editing_queued.is_some() || agent_harness::leading_command(text).is_some() {
            return Vec::new();
        }
        let on_chip = self.editor_context.shown().cloned();
        self.refresh_editor_context(cx);
        let state = &mut self.editor_context;
        if state.hidden {
            return Vec::new();
        }
        // A context that changed since the chip last showed it waits for the next message, once the chip shows it.
        let Some(shown) = state
            .shown()
            .filter(|shown| Some(*shown) == on_chip.as_ref())
            .cloned()
        else {
            return Vec::new();
        };
        let Some(context) = shown.full(cx) else {
            return Vec::new();
        };
        state.sent = Some(shown);
        cx.notify();
        vec![context_block(&context)]
    }

    /// The agent forgets an edited message, so its context goes out again.
    pub(super) fn forget_sent_editor_context(&mut self) {
        self.editor_context.sent = None;
    }

    pub(super) fn toggle_editor_context(&mut self, cx: &mut Context<Self>) {
        self.editor_context.hidden = !self.editor_context.hidden;
        cx.notify();
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
        let label: SharedString = match &context.selection {
            Some((_, start, end)) if start == end => t!("1 line selected"),
            Some((_, start, end)) => t!("{count} lines selected", count = end - start + 1),
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
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_editor_context(cx))),
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
    fn mention_formats() {
        for (path, lines, expected) in [
            ("src/a.rs", Some((5, 10)), "@src/a.rs#5-10"),
            ("src/a.rs", Some((5, 5)), "@src/a.rs#5"),
            ("src/a.rs", None, "@src/a.rs"),
            ("my file.ts", Some((5, 10)), "@\"my file.ts#5-10\""),
            ("C:/x/a.rs", None, "@\"C:/x/a.rs\""),
            ("a#b.rs", Some((1, 2)), "@a#b.rs"),
        ] {
            assert_eq!(
                mention_text_with_lines(path, lines),
                expected,
                "{path} {lines:?}"
            );
        }
    }

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
