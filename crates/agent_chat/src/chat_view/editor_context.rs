use super::{ChatView, completion::relative_mention_path};
use crate::{
    AgentChatSettings, AgentKind,
    chat_style::{icon, ink},
};
use anyhow::Context as _;
use editor::{Anchor, CodeActionProvider, Editor, EditorEvent, ToPoint as _};
use gpui::{
    AnyElement, App, Context, Entity, EntityId, IntoElement, ParentElement, SharedString, Styled,
    Subscription, Task, WeakEntity, Window, div, px,
};
use i18n::t;
use language::{Buffer, DiagnosticSeverity, File as _, LanguageServerId, Point};
use project::{CodeAction, LspAction, Project, ProjectTransaction};
use settings::Settings as _;
use std::{
    ops::Range,
    path::{Path, PathBuf},
    sync::Arc,
};
use terminal::Terminal;
use ui::{IconName, Tooltip, prelude::*};
use workspace::{MultiWorkspace, Workspace};

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
    /// What "Fix with Claude" put in the composer: its mention and the block that goes with it if the next message still mentions it.
    fixes: Vec<(String, String)>,
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

pub(crate) fn applied_line_suffix(relative_path: &str, lines: Option<(u32, u32)>) -> String {
    match lines {
        Some((start, end)) if !relative_path.contains(['"', '#']) => {
            if start == end {
                format!("#{start}")
            } else {
                format!("#{start}-{end}")
            }
        }
        _ => String::new(),
    }
}

/// Quoting and the line suffix follow Claude Code: a path with whitespace or `:` is quoted with the suffix inside, and one containing `"` or `#` gets no suffix since the suffix could not be told apart.
pub(crate) fn mention_text_with_lines(relative_path: &str, lines: Option<(u32, u32)>) -> String {
    quoted_mention(relative_path, &applied_line_suffix(relative_path, lines))
}

fn quoted_mention(relative_path: &str, suffix: &str) -> String {
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
    Some((anchors, range.start.row + 1, last_line(&range)))
}

/// The 1-based last line of `range`. A range of whole lines ends at the start of the next line, which it doesn't cover.
fn last_line(range: &Range<Point>) -> u32 {
    if range.end.column == 0 && range.end.row > range.start.row {
        range.end.row
    } else {
        range.end.row + 1
    }
}

/// The mention `fix_prompt` writes, with the `#L` suffix Claude Code uses for problems.
fn fix_mention(relative_path: &str, diagnostics: &[(u32, u32, String, String)]) -> String {
    let start = diagnostics
        .iter()
        .map(|problem| problem.0)
        .min()
        .unwrap_or(1);
    let end = diagnostics
        .iter()
        .map(|problem| problem.1)
        .max()
        .unwrap_or(start);
    let suffix = if relative_path.contains(['"', '#']) {
        String::new()
    } else {
        format!("#L{start}-{end}")
    };
    quoted_mention(relative_path, &suffix)
}

/// Each diagnostic is (start line, end line, severity, message), lines 1-based. Returns the composer text and the context block.
pub(crate) fn fix_prompt(
    relative_path: &str,
    diagnostics: &[(u32, u32, String, String)],
) -> (String, String) {
    let what = if diagnostics.len() == 1 {
        "this problem"
    } else {
        "these problems"
    };
    let attachment = diagnostics
        .iter()
        .map(|(line, _, severity, message)| format!("{severity}: {message} (line {line})"))
        .collect::<Vec<_>>()
        .join("\n");
    (
        format!("Fix {what} in {} ", fix_mention(relative_path, diagnostics)),
        attachment,
    )
}

/// The errors and warnings in `range`, as `fix_prompt` takes them.
fn fixable_problems(
    buffer: &Buffer,
    range: Range<language::Anchor>,
) -> Vec<(u32, u32, String, String)> {
    buffer
        .snapshot()
        .diagnostics_in_range::<_, Point>(range, false)
        .filter(|entry| entry.diagnostic.is_primary)
        .filter_map(|entry| {
            let severity = match entry.diagnostic.severity {
                DiagnosticSeverity::ERROR => "error",
                DiagnosticSeverity::WARNING => "warning",
                _ => return None,
            };
            Some((
                entry.range.start.row + 1,
                last_line(&entry.range),
                severity.to_string(),
                entry.diagnostic.message.to_string(),
            ))
        })
        .collect()
}

/// The "Fix with Claude" code action, offered on errors and warnings.
pub(crate) struct FixWithClaude;

impl CodeActionProvider for FixWithClaude {
    fn id(&self) -> Arc<str> {
        "fix_with_claude".into()
    }

    fn code_actions(
        &self,
        buffer: &Entity<Buffer>,
        range: Range<language::Anchor>,
        _window: &mut Window,
        cx: &mut App,
    ) -> Task<anyhow::Result<Vec<CodeAction>>> {
        let settings = AgentChatSettings::get_global(cx);
        let buffer = buffer.read(cx);
        let offered = settings.enabled
            && settings.show_fix_actions
            && project::File::from_dyn(buffer.file())
                .and_then(|file| file.as_local())
                .is_some()
            && !fixable_problems(buffer, range.clone()).is_empty();
        let action = offered.then(|| CodeAction {
            server_id: LanguageServerId(0),
            range,
            lsp_action: LspAction::Action(Box::new(lsp::CodeAction {
                title: t!("Fix with Claude").to_string(),
                kind: Some(lsp::CodeActionKind::QUICKFIX),
                ..Default::default()
            })),
            resolved: true,
        });
        Task::ready(Ok(action.into_iter().collect()))
    }

    fn apply_code_action(
        &self,
        buffer: Entity<Buffer>,
        action: CodeAction,
        _push_to_history: bool,
        window: &mut Window,
        cx: &mut App,
    ) -> Task<anyhow::Result<ProjectTransaction>> {
        let workspace = window
            .root::<MultiWorkspace>()
            .flatten()
            .map(|multi_workspace| multi_workspace.read(cx).workspace().clone());
        // The editor applying the action is still being updated, so revealing the chat waits until it's done.
        window.spawn(cx, async move |cx| {
            let workspace = workspace.context("no workspace to fix in")?;
            workspace.update_in(cx, |workspace, window, cx| {
                let buffer = buffer.read(cx);
                let problems = fixable_problems(buffer, action.range);
                let path = project::File::from_dyn(buffer.file())
                    .and_then(|file| Some(file.as_local()?.abs_path(cx)));
                let (Some(path), false) = (path, problems.is_empty()) else {
                    return;
                };
                if let Some(chat) = crate::reveal_last_chat(workspace, window, cx) {
                    chat.update(cx, |chat, cx| {
                        chat.prefill_fix(&path, &problems, window, cx)
                    });
                }
            })?;
            Ok(ProjectTransaction::default())
        })
    }
}

/// Claude Code's wrapper for a mentioned terminal's output, copied from the VS Code extension.
const TERMINAL_BLOCK_TEMPLATE: &str = "<terminal name=\"{name}\">\n{text}\n</terminal>";
const TERMINAL_MENTION_PREFIX: &str = "@terminal:";
/// Claude Code sends the last 100 lines of a mentioned terminal.
const TERMINAL_CONTEXT_LINES: usize = 100;

/// Claude Code writes terminal mentions unquoted with `_` for spaces, so a mention ends at the first whitespace. `"` is dropped because the name goes inside the block's `name="…"`.
pub(crate) fn terminal_mention_name(title: &str) -> String {
    title.replace(char::is_whitespace, "_").replace('"', "")
}

/// Mention names for terminals with these titles, in order. Plain shells in one folder share a title, so later ones get `_2`, `_3`… and each mention names exactly one terminal.
fn unique_mention_names(titles: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for title in titles {
        let base = terminal_mention_name(&title);
        let mut name = base.clone();
        let mut count = 1;
        while names.contains(&name) {
            count += 1;
            name = format!("{base}_{count}");
        }
        names.push(name);
    }
    names
}

/// The open terminals by mention name, in the project's order so the completion menu and sending agree on which terminal a name means.
pub(super) fn open_terminals(
    project: &Entity<Project>,
    cx: &App,
) -> Vec<(String, Entity<Terminal>)> {
    let terminals: Vec<Entity<Terminal>> = project
        .read(cx)
        .local_terminal_handles()
        .iter()
        .filter_map(WeakEntity::upgrade)
        .collect();
    unique_mention_names(
        terminals
            .iter()
            .map(|terminal| terminal.read(cx).title(false)),
    )
    .into_iter()
    .zip(terminals)
    .collect()
}

/// Terminal output goes only to Claude, and never with a slash command or a queued-message edit.
fn sends_terminal_context(kind: AgentKind, editing_queued: bool, text: &str) -> bool {
    kind == AgentKind::Claude && !editing_queued && agent_harness::leading_command(text).is_none()
}

fn recent_output(terminal: &Terminal) -> Option<String> {
    let content = terminal.get_content();
    let lines: Vec<&str> = content.trim_end().lines().collect();
    let output = lines[lines.len().saturating_sub(TERMINAL_CONTEXT_LINES)..].join("\n");
    (!output.trim().is_empty()).then_some(output)
}

/// A block for each terminal `text` mentions, whose output `read` returns by mention name. Punctuation may follow a mention, as in `@terminal:build?`.
fn terminal_context(text: &str, mut read: impl FnMut(&str) -> Option<String>) -> Vec<String> {
    let mut sent: Vec<&str> = Vec::new();
    let mut blocks = Vec::new();
    for (index, _) in text.match_indices(TERMINAL_MENTION_PREFIX) {
        let token = text[index + TERMINAL_MENTION_PREFIX.len()..]
            .split(char::is_whitespace)
            .next()
            .unwrap_or_default();
        let trimmed = token.trim_end_matches(|character: char| character.is_ascii_punctuation());
        if sent.contains(&token) || sent.contains(&trimmed) {
            continue;
        }
        let found = [token, trimmed]
            .into_iter()
            .filter(|name| !name.is_empty())
            .find_map(|name| Some((name, read(name)?)));
        if let Some((name, output)) = found {
            sent.push(name);
            let (head, tail) = TERMINAL_BLOCK_TEMPLATE
                .split_once("{text}")
                .unwrap_or_default();
            blocks.push(format!("{}{output}{tail}", head.replace("{name}", name)));
        }
    }
    blocks
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

    /// The "Fix with Claude" blocks whose mention the message still has. Every send clears them, commands included, so none outlives the message it was made for.
    pub(super) fn take_fix_context(&mut self, text: &str) -> Vec<String> {
        let fixes = std::mem::take(&mut self.editor_context.fixes);
        if self.editing_queued.is_some() || agent_harness::leading_command(text).is_some() {
            return Vec::new();
        }
        fixes
            .into_iter()
            .filter(|(mention, _)| text.contains(mention.as_str()))
            .map(|(_, block)| block)
            .collect()
    }

    /// The output of each open terminal the message mentions, read now. Terminal output can hold secrets, so only a mention in the message itself sends it.
    pub(super) fn mentioned_terminal_context(&self, text: &str, cx: &App) -> Vec<String> {
        if !sends_terminal_context(
            self.session.read(cx).kind(),
            self.editing_queued.is_some(),
            text,
        ) {
            return Vec::new();
        }
        let terminals = open_terminals(&self.project, cx);
        terminal_context(text, |name| {
            let (_, terminal) = terminals.iter().find(|(open, _)| open == name)?;
            recent_output(terminal.read(cx))
        })
    }

    /// Fills the composer with a request to fix `problems` in `path`, never sending it.
    pub(crate) fn prefill_fix(
        &mut self,
        path: &Path,
        problems: &[(u32, u32, String, String)],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let cwd = self.session.read(cx).metadata().cwd.clone();
        let relative_path = relative_mention_path(path, false, &cwd);
        let (text, block) = fix_prompt(&relative_path, problems);
        self.insert_text_at_cursor(text.trim_end(), window, cx);
        self.editor_context
            .fixes
            .push((fix_mention(&relative_path, problems), block));
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
    fn fix_prompt_single_and_plural() {
        let error = (3, 3, "error".to_string(), "mismatched types".to_string());
        let warning = (7, 8, "warning".to_string(), "unused variable".to_string());
        assert_eq!(
            fix_prompt("src/a.rs", std::slice::from_ref(&error)),
            (
                "Fix this problem in @src/a.rs#L3-3 ".to_string(),
                "error: mismatched types (line 3)".to_string()
            )
        );
        assert_eq!(
            fix_prompt("src/a.rs", &[warning, error.clone()]),
            (
                "Fix these problems in @src/a.rs#L3-8 ".to_string(),
                "warning: unused variable (line 7)\nerror: mismatched types (line 3)".to_string()
            )
        );
        for (path, expected) in [
            ("my file.rs", "Fix this problem in @\"my file.rs#L3-3\" "),
            ("C:/x/a.rs", "Fix this problem in @\"C:/x/a.rs#L3-3\" "),
            ("a#b.rs", "Fix this problem in @a#b.rs "),
        ] {
            assert_eq!(fix_prompt(path, std::slice::from_ref(&error)).0, expected);
        }
    }

    #[test]
    fn terminal_mention_becomes_a_context_block() {
        let read = |name: &str| (name == "npm_run_dev").then(|| "ready on :3000".to_string());
        assert_eq!(terminal_mention_name("npm run dev"), "npm_run_dev");
        assert_eq!(
            terminal_context("what port is in @terminal:npm_run_dev?", read),
            ["<terminal name=\"npm_run_dev\">\nready on :3000\n</terminal>"]
        );
        assert_eq!(
            terminal_context("@terminal:npm_run_dev and @terminal:npm_run_dev", read).len(),
            1
        );
        assert!(terminal_context("@terminal:other", read).is_empty());
        assert!(
            terminal_context("what port is in npm_run_dev?", |_| {
                panic!("a terminal is read only when the message mentions it")
            })
            .is_empty()
        );
    }

    #[test]
    fn same_titled_terminals_each_get_their_own_mention() {
        let titles = ["wu — pwsh", "wu — pwsh", "build"].map(String::from);
        let outputs = ["first shell", "second shell", "cargo 1.90.0"];
        let names = unique_mention_names(titles);
        assert_eq!(names, ["wu_—_pwsh", "wu_—_pwsh_2", "build"]);
        let read = |name: &str| {
            let index = names.iter().position(|open| open == name)?;
            Some(outputs[index].to_string())
        };
        assert_eq!(
            terminal_context("@terminal:wu_—_pwsh_2 and @terminal:wu_—_pwsh", read),
            [
                "<terminal name=\"wu_—_pwsh_2\">\nsecond shell\n</terminal>",
                "<terminal name=\"wu_—_pwsh\">\nfirst shell\n</terminal>",
            ]
        );
    }

    #[test]
    fn a_mention_reads_only_the_terminal_it_names() {
        let read = |name: &str| ["build2", "build_x"].contains(&name).then(String::new);
        assert!(terminal_context("@terminal:build", read).is_empty());
        assert_eq!(terminal_mention_name("say \"hi\""), "say_hi");
    }

    #[test]
    fn terminals_are_read_only_for_plain_claude_messages() {
        let text = "what failed in @terminal:build?";
        assert!(sends_terminal_context(AgentKind::Claude, false, text));
        assert!(!sends_terminal_context(AgentKind::Codex, false, text));
        assert!(!sends_terminal_context(AgentKind::Claude, true, text));
        assert!(!sends_terminal_context(
            AgentKind::Claude,
            false,
            "/review @terminal:build"
        ));
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
