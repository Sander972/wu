use super::editor_context::{applied_line_suffix, mention_text_with_lines, open_terminals};
use super::{ChatView, claude_dialogs::show_toast, ui};
use crate::{
    AcceptCommand, AgentKind, DismissCommands, NewAntigravityChat, NewClaudeChat, NewCodexChat,
    NewOpencodeChat, SelectNextCommand, SelectPreviousCommand, Send, ToggleFocus, ToggleFocusView,
    agent_panel::AgentPanel,
    chat_style::{MENU_ITEM_RADIUS, icon, ink, popover_card, selected_row, working_animation},
    slash_commands::{
        AppCommand, CommandItem, CommandSection, CommandTarget, CommandToken, ISSUE_URL, TokenKind,
        argument_error, command_items, completion_token, filter_commands, hidden_command_target,
    },
    usage_rings::menu_heading,
};
use agent_harness::{PermissionMode, SkillRef};
use editor::{FoldPlaceholder, MultiBufferOffset, SelectionEffects, display_map::Crease};
use file_icons::FileIcons;
use gpui::{
    AnimationExt as _, AnyElement, App, Context, Corners, Focusable as _, FontWeight, Hsla,
    IntoElement, ParentElement, SharedString, Styled, Window, canvas, deferred, div,
    pulsating_between, px, relative,
};
use i18n::t;
use project::{Candidates, PathMatchCandidateSet, ProjectPath};
use std::{
    ops::Range,
    path::{Path, PathBuf},
    sync::{Arc, atomic::AtomicBool},
    time::Duration,
};
use ui::{IconName, prelude::*};
use util::{ResultExt as _, paths::compare_rel_paths, rel_path::RelPath};

const MAX_MENTION_RESULTS: usize = 50;

pub(super) struct CompletionMenu {
    pub kind: TokenKind,
    pub token: CommandToken,
    pub selected: usize,
}

#[derive(Clone)]
pub(super) struct MentionItem {
    pub path: PathBuf,
    pub label: String,
    pub detail: String,
    pub is_dir: bool,
}

#[derive(Clone)]
enum Row {
    Command(CommandItem),
    Skill(agent_harness::Skill),
    Mention(MentionItem),
    /// An open terminal, by its mention name.
    Terminal(String),
}

impl Row {
    fn icon(&self, size: f32, cx: &App) -> AnyElement {
        let name = match self {
            Row::Command(item) if item.is_skill => IconName::AgentWidget,
            Row::Command(_) => IconName::AgentCommand,
            Row::Skill(_) => IconName::AgentWidget,
            Row::Terminal(_) => IconName::Terminal,
            Row::Mention(item) => {
                return file_icon(&item.path, item.is_dir, cx)
                    .size(IconSize::Custom(ui(size)))
                    .into_any_element();
            }
        };
        icon(name, px(size), cx.theme().colors().text_muted).into_any_element()
    }

    fn section(&self) -> Option<CommandSection> {
        match self {
            Row::Command(item) => item.section,
            Row::Skill(_) | Row::Mention(_) | Row::Terminal(_) => None,
        }
    }

    fn label(&self) -> String {
        match self {
            Row::Command(item) => format!("/{}", item.name),
            Row::Skill(skill) => skill_display_name(&skill.name),
            Row::Mention(item) => item.label.clone(),
            Row::Terminal(name) => name.clone(),
        }
    }

    fn detail(&self) -> String {
        match self {
            Row::Command(item) => item.detail.clone(),
            Row::Skill(skill) => skill.description.clone(),
            Row::Mention(item) => item.detail.clone(),
            Row::Terminal(_) => String::new(),
        }
    }
}

fn skill_display_name(name: &str) -> String {
    name.rsplit(':')
        .next()
        .unwrap_or(name)
        .split(['-', '_', ' '])
        .filter(|word| !word.is_empty())
        .map(|word| {
            let mut characters = word.chars();
            characters
                .next()
                .map(|first| first.to_uppercase().collect::<String>() + characters.as_str())
                .unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

pub(super) fn relative_mention_path(path: &Path, is_dir: bool, cwd: &Path) -> String {
    let mut shown = path
        .strip_prefix(cwd)
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned();
    if cfg!(windows) {
        shown = shown.replace('\\', "/");
    }
    if is_dir && !shown.ends_with('/') {
        shown.push('/');
    }
    shown
}

fn mention_text(path: &Path, is_dir: bool, cwd: &Path, lines: Option<(u32, u32)>) -> String {
    mention_text_with_lines(&relative_mention_path(path, is_dir, cwd), lines)
}

fn follows_word(editor: &mut editor::Editor, cx: &mut Context<editor::Editor>) -> bool {
    let snapshot = editor.display_snapshot(cx);
    let start = editor
        .selections
        .newest::<MultiBufferOffset>(&snapshot)
        .start
        .0;
    editor
        .text(cx)
        .get(..start)
        .and_then(|before| before.chars().next_back())
        .is_some_and(|character| !character.is_whitespace())
}

/// Splits a typed `#5` or `#5-10` off the end of a mention query.
fn split_line_suffix(query: &str) -> (&str, Option<(u32, u32)>) {
    let Some((path, range)) = query.rsplit_once('#') else {
        return (query, None);
    };
    let (start, end) = range.split_once('-').unwrap_or((range, range));
    match (start.parse::<u32>(), end.parse::<u32>()) {
        (Ok(start), Ok(end)) if start >= 1 && end >= start => (path, Some((start, end))),
        _ => (query, None),
    }
}

fn file_icon(path: &Path, is_dir: bool, cx: &App) -> Icon {
    let theme_icon = if is_dir {
        FileIcons::get_folder_icon(false, path, cx)
    } else {
        FileIcons::get_icon(path, cx)
    };
    match theme_icon {
        Some(theme_icon) => Icon::from_path(theme_icon),
        None if is_dir => Icon::new(IconName::AgentFolder),
        None => Icon::new(IconName::AgentDocument),
    }
    .color(Color::Muted)
}

fn badge(row: Row) -> FoldPlaceholder {
    let label: SharedString = row.label().into();
    FoldPlaceholder {
        render: Arc::new(move |fold_id, _, cx| {
            div()
                .id(fold_id)
                .h_full()
                .flex()
                .items_center()
                .child(
                    h_flex()
                        .h(ui(20.))
                        .gap(px(4.))
                        .px(px(5.))
                        .rounded(px(6.))
                        .bg(ink(0.07, cx))
                        .text_size(ui(13.))
                        .text_color(cx.theme().colors().text)
                        .child(row.icon(14., cx))
                        .child(label.clone()),
                )
                .into_any_element()
        }),
        constrain_width: false,
        merge_adjacent: false,
        ..FoldPlaceholder::default()
    }
}

fn mention_item(path: PathBuf, is_dir: bool, cwd: &Path) -> MentionItem {
    let shown = path.strip_prefix(cwd).unwrap_or(&path).to_path_buf();
    let label = shown
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| shown.to_string_lossy().into_owned());
    let detail = shown
        .parent()
        .map(|parent| parent.to_string_lossy().into_owned())
        .unwrap_or_default();
    MentionItem {
        path,
        label,
        detail,
        is_dir,
    }
}

impl ChatView {
    fn session_kind_and_cwd(&self, cx: &App) -> (AgentKind, PathBuf) {
        let session = self.session.read(cx);
        (session.kind(), session.metadata().cwd.clone())
    }

    pub(super) fn command_items(&self, cx: &App) -> Vec<CommandItem> {
        let (kind, cwd) = self.session_kind_and_cwd(cx);
        let catalog = self.store.read(cx).commands(kind, &cwd);
        let mut items = command_items(catalog.map_or(&[], |catalog| catalog.commands.as_slice()));
        // Other agents have no plan mode, which would quietly fall back to Auto, no file checkpoints and no Claude Code dialogs.
        if kind != AgentKind::Claude {
            items.retain(|item| {
                !matches!(
                    item.target,
                    CommandTarget::App(
                        AppCommand::Plan { .. }
                            | AppCommand::Rewind
                            | AppCommand::Status
                            | AppCommand::Hooks
                            | AppCommand::Memory
                            | AppCommand::Skills
                            | AppCommand::Sandbox
                            | AppCommand::OutputStyle
                            | AppCommand::Mcp
                            | AppCommand::Permissions
                            | AppCommand::Plugins
                            | AppCommand::Usage
                            | AppCommand::Export { .. }
                            | AppCommand::Btw { .. }
                            | AppCommand::Login
                            | AppCommand::Logout
                            | AppCommand::Tasks
                    )
                )
            });
        }
        items
    }

    fn rows(&self, cx: &App) -> Vec<Row> {
        let Some(menu) = &self.command_menu else {
            return Vec::new();
        };
        match menu.kind {
            TokenKind::Command => filter_commands(&menu.token.query, self.command_items(cx))
                .into_iter()
                .map(Row::Command)
                .collect(),
            TokenKind::Skill => {
                let query = menu.token.query.to_lowercase();
                let mut skills: Vec<(usize, agent_harness::Skill)> = self
                    .available_skills(cx)
                    .0
                    .into_iter()
                    .filter(|skill| skill.enabled)
                    .filter_map(|skill| {
                        let name = skill.name.to_lowercase();
                        let rank = if name.starts_with(&query) {
                            0
                        } else if name.contains(&query) {
                            1
                        } else {
                            return None;
                        };
                        Some((rank, skill))
                    })
                    .collect();
                skills.sort_by_key(|(rank, _)| *rank);
                skills.into_iter().map(|(_, skill)| Row::Skill(skill)).collect()
            }
            TokenKind::Mention => {
                // Only Claude receives a mentioned terminal's output.
                let terminals = if self.session.read(cx).kind() == AgentKind::Claude {
                    open_terminals(&self.project, cx)
                } else {
                    Vec::new()
                };
                let query = menu.token.query.to_lowercase();
                terminals
                    .into_iter()
                    .map(|(name, _)| name)
                    .filter(|name| format!("terminal:{name}").to_lowercase().contains(&query))
                    .map(Row::Terminal)
                    .chain(self.mention_results.iter().cloned().map(Row::Mention))
                    .collect()
            }
        }
    }

    /// Claude Code reports its skills as slash commands.
    fn available_skills(&self, cx: &App) -> (Vec<agent_harness::Skill>, bool) {
        let (kind, cwd) = self.session_kind_and_cwd(cx);
        let store = self.store.read(cx);
        if kind == AgentKind::Claude {
            let Some(catalog) = store.commands(kind, &cwd) else {
                return (Vec::new(), false);
            };
            let skills = catalog
                .commands
                .iter()
                .filter(|command| command.is_skill)
                .map(|command| agent_harness::Skill {
                    name: command.name.clone(),
                    description: command.description.clone(),
                    path: Some(format!("claude-skill:{}", command.name)),
                    enabled: true,
                })
                .collect();
            return (skills, catalog.loaded);
        }
        store
            .skills(kind, &cwd)
            .map_or((Vec::new(), false), |catalog| {
                (catalog.skills.clone(), catalog.loaded)
            })
    }

    #[cfg(test)]
    pub(super) fn completion_names(&self, cx: &App) -> Vec<String> {
        self.rows(cx)
            .into_iter()
            .map(|row| match row {
                Row::Command(item) => item.name,
                Row::Skill(skill) => skill.name,
                Row::Mention(item) => item.label,
                Row::Terminal(name) => name,
            })
            .collect()
    }

    #[cfg(test)]
    pub(crate) fn composer_text_for_test(&self, cx: &App) -> String {
        self.composer.read(cx).text(cx)
    }

    #[cfg(test)]
    pub(crate) fn command_menu_is_open_for_test(&self) -> bool {
        self.command_menu.is_some()
    }

    /// Adds a link's prompt after any draft, leaving it for the user to edit and send: the command and mention menus stay closed, so pressing Enter can't pick a command the link chose.
    pub(crate) fn place_link_prompt(
        &mut self,
        prompt: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let token = self.composer.update(cx, |editor, cx| {
            let separator = if editor.is_empty(cx) { "" } else { "\n\n" };
            editor.move_to_end(&editor::actions::MoveToEnd, window, cx);
            editor.insert(&format!("{separator}{prompt}"), window, cx);
            let snapshot = editor.display_snapshot(cx);
            let cursor = editor
                .selections
                .newest::<MultiBufferOffset>(&snapshot)
                .head()
                .0;
            completion_token(&editor.text(cx), cursor)
        });
        self.dismissed_command = token.map(|(_, token)| token);
        self.command_menu = None;
        window.focus(&self.composer.focus_handle(cx), cx);
    }

    #[cfg(test)]
    pub(crate) fn set_composer_text_for_test(
        &mut self,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.composer
            .update(cx, |editor, cx| editor.set_text(text, window, cx));
    }

    pub(super) fn completion_count(&self, cx: &App) -> usize {
        self.rows(cx).len()
    }

    pub(super) fn update_command_menu(&mut self, cx: &mut Context<Self>) {
        let token = self.composer.update(cx, |editor, cx| {
            let snapshot = editor.display_snapshot(cx);
            let selection = editor.selections.newest::<MultiBufferOffset>(&snapshot);
            if !selection.is_empty() {
                return None;
            }
            completion_token(&editor.text(cx), selection.head().0)
        });
        let Some((kind, token)) = token else {
            self.dismissed_command = None;
            if self.command_menu.take().is_some() {
                cx.notify();
            }
            return;
        };
        if self.dismissed_command.as_ref() == Some(&token) {
            return;
        }
        self.dismissed_command = None;
        let query_changed = match &mut self.command_menu {
            Some(menu) if menu.token == token && menu.kind == kind => return,
            Some(menu) if menu.kind == kind => {
                let changed = menu.token.query != token.query;
                if changed {
                    menu.selected = 0;
                }
                menu.token = token.clone();
                changed
            }
            _ => {
                let (agent, cwd) = self.session_kind_and_cwd(cx);
                self.store.update(cx, |store, cx| match kind {
                    TokenKind::Command => store.refresh_commands(agent, cwd, cx),
                    TokenKind::Skill if agent == AgentKind::Claude => {
                        store.refresh_commands(agent, cwd, cx)
                    }
                    TokenKind::Skill => store.refresh_skills(agent, cwd, cx),
                    TokenKind::Mention => {}
                });
                self.command_menu = Some(CompletionMenu {
                    kind,
                    token: token.clone(),
                    selected: 0,
                });
                true
            }
        };
        if query_changed {
            self.command_scroll.scroll_to_item(0);
            if kind == TokenKind::Mention {
                self.search_mentions(split_line_suffix(&token.query).0.to_string(), cx);
            }
        }
        cx.notify();
    }

    fn recent_mentions(
        &self,
        history: Vec<(ProjectPath, Option<PathBuf>)>,
        cwd: &Path,
        cx: &App,
    ) -> Vec<MentionItem> {
        let project = self.project.read(cx);
        history
            .into_iter()
            .filter(|(project_path, _)| {
                project
                    .entry_for_path(project_path, cx)
                    .is_some_and(|entry| entry.is_file())
            })
            .filter_map(|(project_path, abs_path)| {
                abs_path.or_else(|| project.absolute_path(&project_path, cx))
            })
            .take(MAX_MENTION_RESULTS)
            .map(|path| mention_item(path, false, cwd))
            .collect()
    }

    fn root_mentions(&self, cwd: &Path, cx: &App) -> Vec<MentionItem> {
        self.project
            .read(cx)
            .visible_worktrees(cx)
            .flat_map(|worktree| {
                let worktree = worktree.read(cx);
                let mut entries: Vec<_> = worktree
                    .child_entries(RelPath::empty())
                    .filter(|entry| !entry.is_ignored)
                    .collect();
                entries.sort_by(|a, b| {
                    compare_rel_paths((&a.path, a.is_file()), (&b.path, b.is_file()))
                });
                entries
                    .into_iter()
                    .map(|entry| {
                        let path = worktree.abs_path().join(entry.path.as_std_path());
                        mention_item(path, entry.is_dir(), cwd)
                    })
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    fn search_mentions(&mut self, query: String, cx: &mut Context<Self>) {
        let cwd = self.session.read(cx).metadata().cwd.clone();
        if query.is_empty() {
            let workspace = self.workspace.clone();
            // The chat is itself a workspace item, so reading the history while it updates panics.
            self._mention_search = cx.spawn(async move |this, cx| {
                let history = workspace
                    .read_with(cx, |workspace, cx| {
                        workspace.recent_navigation_history(None, cx)
                    })
                    .unwrap_or_default();
                this.update(cx, |this, cx| {
                    let recent = this.recent_mentions(history, &cwd, cx);
                    this.mention_results = if recent.is_empty() {
                        this.root_mentions(&cwd, cx)
                    } else {
                        recent
                    };
                    cx.notify();
                })
                .log_err();
            });
            return;
        }
        let project = self.project.read(cx);
        let worktrees: Vec<_> = project.visible_worktrees(cx).collect();
        let roots: Vec<(usize, PathBuf)> = worktrees
            .iter()
            .map(|worktree| {
                let worktree = worktree.read(cx);
                (worktree.id().to_usize(), worktree.abs_path().to_path_buf())
            })
            .collect();
        let candidate_sets: Vec<PathMatchCandidateSet> = worktrees
            .iter()
            .map(|worktree| PathMatchCandidateSet {
                snapshot: worktree.read(cx).snapshot(),
                include_ignored: false,
                include_root_name: false,
                candidates: Candidates::Entries,
            })
            .collect();
        self._mention_search = cx.spawn(async move |this, cx| {
            let cancel = AtomicBool::new(false);
            let matches = fuzzy_nucleo::match_path_sets(
                candidate_sets.as_slice(),
                &query,
                &None,
                fuzzy_nucleo::Case::Ignore,
                MAX_MENTION_RESULTS,
                &cancel,
                cx.background_executor().clone(),
            )
            .await;
            let mut items: Vec<MentionItem> = matches
                .into_iter()
                .filter_map(|found| {
                    let root = roots
                        .iter()
                        .find(|(id, _)| *id == found.worktree_id)
                        .map(|(_, root)| root)?;
                    let path = root.join(found.path.as_std_path());
                    Some(mention_item(path, found.is_dir, &cwd))
                })
                .collect();
            items.sort_by_key(|item| item.is_dir);
            this.update(cx, |this, cx| {
                this.mention_results = items;
                cx.notify();
            })
            .log_err();
        });
    }

    fn move_command_selection(&mut self, step: isize, cx: &mut Context<Self>) {
        let count = self.completion_count(cx);
        let Some(menu) = &mut self.command_menu else {
            return;
        };
        if count == 0 {
            return;
        }
        menu.selected = (menu.selected as isize + step).rem_euclid(count as isize) as usize;
        self.command_scroll.scroll_to_item(menu.selected);
        cx.notify();
    }

    pub(super) fn select_previous_command(
        &mut self,
        _: &SelectPreviousCommand,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.move_command_selection(-1, cx);
    }

    pub(super) fn select_next_command(
        &mut self,
        _: &SelectNextCommand,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.move_command_selection(1, cx);
    }

    pub(super) fn dismiss_commands(
        &mut self,
        _: &DismissCommands,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(menu) = self.command_menu.take() {
            self.dismissed_command = Some(menu.token);
            cx.notify();
        }
    }

    pub(super) fn accept_command(
        &mut self,
        _: &AcceptCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let selected = self.command_menu.as_ref().map(|menu| menu.selected);
        match selected {
            Some(index) if index < self.completion_count(cx) => {
                self.pick_command(index, window, cx)
            }
            _ => self.send(&Send, window, cx),
        }
    }

    pub(super) fn insert_at_token(
        &mut self,
        range: std::ops::Range<usize>,
        insertion: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let text = self.composer.read(cx).text(cx);
        let next = text.get(range.end..).and_then(|rest| rest.chars().next());
        let insertion = if insertion.is_empty() || next.is_some_and(char::is_whitespace) {
            insertion
        } else {
            format!("{insertion} ")
        };
        self.composer.update(cx, |editor, cx| {
            let cursor = range.start + insertion.len();
            editor.edit(
                [(
                    MultiBufferOffset(range.start)..MultiBufferOffset(range.end),
                    insertion,
                )],
                cx,
            );
            editor.change_selections(SelectionEffects::default(), window, cx, |selections| {
                selections.select_ranges([MultiBufferOffset(cursor)..MultiBufferOffset(cursor)])
            });
        });
    }

    pub(super) fn pick_command(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(mut row) = self.rows(cx).into_iter().nth(index) else {
            return;
        };
        let Some(menu) = self.command_menu.take() else {
            return;
        };
        let lines = split_line_suffix(&menu.token.query).1;
        let range = menu.token.range;
        let cwd = self.session.read(cx).metadata().cwd.clone();
        let text = match &row {
            // Inserted like an agent command so a task can follow it.
            Row::Command(CommandItem {
                target: CommandTarget::App(command),
                ..
            }) if !matches!(command, AppCommand::Plan { .. }) => {
                self.insert_at_token(range, String::new(), window, cx);
                self.run_app_command(command.clone(), window, cx);
                cx.notify();
                return;
            }
            Row::Command(item) => format!("/{}", item.name),
            Row::Skill(skill) => {
                if let Some(path) = &skill.path {
                    let skill_ref = SkillRef {
                        name: skill.name.clone(),
                        path: path.clone(),
                    };
                    if !self.composer_skills.contains(&skill_ref) {
                        self.composer_skills.push(skill_ref);
                    }
                }
                format!("${}", skill.name)
            }
            Row::Mention(item) => mention_text(&item.path, item.is_dir, &cwd, lines),
            Row::Terminal(name) => format!("@terminal:{name}"),
        };
        if let Row::Mention(item) = &mut row {
            let relative = relative_mention_path(&item.path, item.is_dir, &cwd);
            item.label.push_str(&applied_line_suffix(&relative, lines));
        }
        let badge_range = range.start..range.start + text.len();
        self.insert_at_token(range, text, window, cx);
        self.show_badge(badge_range, row, window, cx);
        cx.notify();
    }

    pub(crate) fn insert_mentions(
        &mut self,
        paths: Vec<PathBuf>,
        prefix: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if paths.is_empty() {
            return;
        }
        let cwd = self.session.read(cx).metadata().cwd.clone();
        let mentions: Vec<(PathBuf, bool, String)> = paths
            .into_iter()
            .map(|path| {
                let is_dir = path.is_dir();
                let text = mention_text(&path, is_dir, &cwd, None);
                (path, is_dir, text)
            })
            .collect();
        let text = mentions
            .iter()
            .map(|(_, _, text)| text.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        let end = self.composer.update(cx, |editor, cx| {
            let separator = if prefix.is_empty() || !follows_word(editor, cx) {
                ""
            } else {
                " "
            };
            editor.insert(&format!("{separator}{prefix}{text} "), window, cx);
            let snapshot = editor.display_snapshot(cx);
            editor
                .selections
                .newest::<MultiBufferOffset>(&snapshot)
                .head()
                .0
        });
        let mut start = end.saturating_sub(text.len() + 1);
        for (path, is_dir, text) in mentions {
            let row = Row::Mention(mention_item(path, is_dir, &cwd));
            self.show_badge(start..start + text.len(), row, window, cx);
            start += text.len() + 1;
        }
        window.focus(&self.composer.focus_handle(cx), cx);
    }

    pub(crate) fn insert_file_mention(
        &mut self,
        path: &Path,
        lines: Option<(u32, u32)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let cwd = self.session.read(cx).metadata().cwd.clone();
        let text = mention_text(path, false, &cwd, lines);
        self.insert_text_at_cursor(&text, window, cx);
    }

    pub(crate) fn insert_text_at_cursor(
        &mut self,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.composer.update(cx, |editor, cx| {
            let separator = if follows_word(editor, cx) { " " } else { "" };
            editor.insert(&format!("{separator}{text} "), window, cx);
        });
        window.focus(&self.composer.focus_handle(cx), cx);
    }

    fn show_badge(
        &mut self,
        range: Range<usize>,
        row: Row,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let crease = Crease::simple(
            MultiBufferOffset(range.start)..MultiBufferOffset(range.end),
            badge(row),
        );
        self.composer.update(cx, |editor, cx| {
            editor.fold_creases(vec![crease], false, window, cx)
        });
    }

    pub(crate) fn run_app_command(
        &mut self,
        command: AppCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match command {
            AppCommand::Model => self.model_picker.show(window, cx),
            AppCommand::New => match self.session.read(cx).kind() {
                AgentKind::Claude => window.dispatch_action(Box::new(NewClaudeChat), cx),
                AgentKind::Codex => window.dispatch_action(Box::new(NewCodexChat), cx),
                AgentKind::Opencode => window.dispatch_action(Box::new(NewOpencodeChat), cx),
                AgentKind::Antigravity => window.dispatch_action(Box::new(NewAntigravityChat), cx),
            },
            AppCommand::Resume => window.dispatch_action(Box::new(ToggleFocus), cx),
            AppCommand::Stop => self.session.update(cx, |session, cx| session.stop(cx)),
            AppCommand::Plan { open: true, .. } => self.open_plan(window, cx),
            AppCommand::Plan { task, open: false } => {
                self.session.update(cx, |session, cx| {
                    session
                        .update_settings(|settings| settings.permission = PermissionMode::Plan, cx)
                });
                if let Some(task) = task {
                    self.session
                        .update(cx, |session, cx| session.send_message(task, cx));
                    self.glide_to_bottom(cx);
                }
            }
            AppCommand::Fork { text } => {
                let id = self.session.read(cx).metadata().id.clone();
                let fork = self
                    .store
                    .update(cx, |store, cx| store.fork_session(&id, false, cx));
                self.open_fork(fork, text, window, cx);
            }
            AppCommand::Rewind => {
                let Some(index) = self.session.read(cx).last_user_index() else {
                    return;
                };
                let position = self
                    .composer
                    .read(cx)
                    .last_bounds()
                    .map_or_else(|| window.mouse_position(), |bounds| bounds.origin);
                self.show_rewind_menu(index, position, window, cx);
            }
            AppCommand::Status
            | AppCommand::Hooks
            | AppCommand::Memory
            | AppCommand::Skills
            | AppCommand::Sandbox
            | AppCommand::OutputStyle
            | AppCommand::Mcp
            | AppCommand::Permissions
            | AppCommand::Plugins
            | AppCommand::Usage => self.show_claude_dialog(&command, window, cx),
            AppCommand::Export { file } => self.export_conversation(file, window, cx),
            AppCommand::Copy { nth } => self.copy_reply(nth, cx),
            AppCommand::Btw { question } => self.ask_side_question(question, window, cx),
            AppCommand::Login => self.show_login_dialog(window, cx),
            AppCommand::Logout => self.sign_out(window, cx),
            AppCommand::Bug => cx.open_url(ISSUE_URL),
            AppCommand::Config => match cx.build_action(
                "wu::OpenSettingsPage",
                Some(serde_json::json!({ "page": "Agent Chat" })),
            ) {
                Ok(action) => window.dispatch_action(action, cx),
                Err(error) => log::error!("cannot open the agent chat settings: {error}"),
            },
            AppCommand::Bookmarks => {
                self.workspace
                    .update(cx, |workspace, cx| {
                        workspace.open_panel::<AgentPanel>(window, cx);
                        if let Some(panel) = workspace.panel::<AgentPanel>(cx) {
                            panel.update(cx, |panel, cx| panel.toggle_saved_messages(window, cx));
                        }
                    })
                    .log_err();
            }
            AppCommand::FocusView => self.toggle_focus_view(&ToggleFocusView, window, cx),
            AppCommand::Tasks => self.show_background_tasks(window, cx),
            AppCommand::Settings | AppCommand::Diff | AppCommand::Files | AppCommand::Terminal => {
                let Some(name) = command.action_name() else {
                    return;
                };
                match cx.build_action(name, None) {
                    Ok(action) => window.dispatch_action(action, cx),
                    Err(error) => log::error!("cannot run {name}: {error}"),
                }
            }
        }
    }

    pub(super) fn redirect_hidden_command(
        &mut self,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some((name, _)) = agent_harness::leading_command(text) else {
            return false;
        };
        let items = self.command_items(cx);
        let argument_error = argument_error(text, &items);
        if self.session.read(cx).kind() != AgentKind::Claude
            || !(agent_harness::claude::is_hidden_command(name) || argument_error.is_some())
            || items
                .iter()
                .any(|item| item.target == CommandTarget::Agent && item.name == name)
        {
            return false;
        }
        match hidden_command_target(name) {
            Some(command) => self.run_app_command(command, window, cx),
            None => {
                let message = argument_error.unwrap_or_else(|| {
                    t!(
                        "/{name} only works in Claude Code in the terminal.",
                        name = name
                    )
                });
                show_toast(&self.workspace, message.to_string(), cx);
            }
        }
        true
    }

    pub(super) fn render_command_menu(&self, cx: &Context<Self>) -> AnyElement {
        let Some(menu) = &self.command_menu else {
            return div().into_any_element();
        };
        let colors = cx.theme().colors();
        let (kind, cwd) = self.session_kind_and_cwd(cx);
        let store = self.store.read(cx);
        let (loading, error, empty_message) = match menu.kind {
            TokenKind::Command => {
                let catalog = store.commands(kind, &cwd);
                (
                    catalog.is_none_or(|catalog| catalog.is_loading() && !catalog.loaded),
                    catalog.and_then(|catalog| catalog.error.clone()),
                    t!("No matching commands"),
                )
            }
            TokenKind::Skill => {
                let (skills, loaded) = self.available_skills(cx);
                let message = if loaded && skills.is_empty() {
                    t!("No skills found")
                } else {
                    t!("No matching skills")
                };
                (!loaded, None, message)
            }
            TokenKind::Mention => (false, None, t!("No matching files")),
        };
        let rows = self.rows(cx);
        let message_row = |text: SharedString, color: Hsla| {
            div()
                .px(px(12.))
                .py(px(10.))
                .text_size(ui(12.))
                .text_color(color)
                .child(text)
        };
        let show_sections = menu.kind == TokenKind::Command && menu.token.query.is_empty();
        let mut previous_section = None;
        let headings: Vec<Option<CommandSection>> = rows
            .iter()
            .map(|row| {
                let section = row.section();
                let heading =
                    section.filter(|section| show_sections && previous_section != Some(*section));
                previous_section = section;
                heading
            })
            .collect();
        let row_elements: Vec<AnyElement> = rows
            .into_iter()
            .enumerate()
            .map(|(index, row)| {
                let selected = index == menu.selected;
                let detail = row.detail();
                let has_detail = !detail.is_empty();
                h_flex()
                    .id(("agent-completion", index))
                    .flex_none()
                    .gap(px(8.))
                    .px(px(8.))
                    .py(px(6.))
                    .rounded(px(MENU_ITEM_RADIUS))
                    .cursor_pointer()
                    .text_size(ui(13.))
                    .map(|this| {
                        if selected {
                            this.bg(selected_row(cx))
                        } else {
                            let hover = selected_row(cx);
                            this.hover(move |style| style.bg(hover))
                        }
                    })
                    .child(div().flex_none().size(px(16.)).child(row.icon(16., cx)))
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(colors.text)
                            .map(|this| {
                                if has_detail {
                                    this.flex_none().max_w(relative(0.55))
                                } else {
                                    this.flex_1()
                                }
                            })
                            .child(row.label()),
                    )
                    .when(has_detail, |this| {
                        this.child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_size(ui(12.5))
                                .text_color(colors.text_muted)
                                .child(detail),
                        )
                    })
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.pick_command(index, window, cx)
                    }))
                    .into_any_element()
            })
            .collect();
        // One child per row keeps `scroll_to_item` indexes aligned with the rows.
        let row_elements: Vec<AnyElement> = row_elements
            .into_iter()
            .zip(headings)
            .map(|(element, heading)| {
                v_flex()
                    .flex_none()
                    .when_some(heading, |this, section| {
                        this.child(menu_heading(&section.title(), cx))
                    })
                    .child(element)
                    .into_any_element()
            })
            .collect();
        let empty = row_elements.is_empty();
        let card = popover_card(cx)
            .relative()
            .w_full()
            .max_h(px(320.))
            .bg(colors.elevated_surface_background.opacity(0.85))
            .child(
                canvas(
                    |_, _, _| (),
                    |bounds, (), window, _| {
                        window.paint_backdrop_blur(bounds, Corners::all(px(11.)), px(16.))
                    },
                )
                .absolute()
                .inset_0(),
            )
            .when_some(error, |this, error| {
                this.child(message_row(error, cx.theme().status().error))
            })
            .map(|this| {
                if empty && loading {
                    this.child(v_flex().py(px(4.)).gap(px(6.)).children((0..3usize).map(
                        |index| {
                            div()
                                .h(px(28.))
                                .rounded(px(6.))
                                .bg(ink(0.04, cx))
                                .with_animation(
                                    ("agent-completion-skeleton", index),
                                    working_animation(Duration::from_millis(1400))
                                        .with_easing(pulsating_between(0.35, 0.75)),
                                    |this, delta| this.opacity(delta),
                                )
                        },
                    )))
                } else if empty {
                    this.child(message_row(empty_message, colors.text_muted))
                } else {
                    this.child(
                        v_flex()
                            .id("agent-completion-list")
                            .max_h(px(310.))
                            .overflow_y_scroll()
                            .track_scroll(&self.command_scroll)
                            .gap(px(2.))
                            .children(row_elements),
                    )
                }
            });
        deferred(
            div()
                .absolute()
                .bottom_full()
                .left_0()
                .right_0()
                .pb(px(6.))
                .occlude()
                .child(card),
        )
        .with_priority(1)
        .into_any_element()
    }
}
