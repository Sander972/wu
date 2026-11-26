mod agent_chat_settings;
mod agent_panel;
mod chat_panel;
mod chat_style;
mod chat_view;
mod commit_message;
mod ide_server;
mod model_picker;
mod notifications;
mod saved_chats;
mod session;
mod slash_commands;
mod usage_rings;

pub use agent_chat_settings::{AgentChatSettings, CommitMessageSettings};
pub use agent_panel::AgentPanel;
pub use chat_panel::ChatPanel;
pub use chat_view::ChatView;
pub use commit_message::{
    commit_message_agent_label, commit_message_menu, generate_commit_message,
};

use command_palette_hooks::CommandPaletteFilter;
use futures::FutureExt as _;
use gpui::{
    App, AppContext as _, Context, Entity, EntityId, Global, PromptButton, PromptLevel,
    TaskExt as _, WeakEntity, Window, WindowHandle, actions,
};
use i18n::t;
use serde::{Deserialize, Serialize};
use settings::{Settings as _, SettingsStore};
use std::path::{Path, PathBuf};
use util::ResultExt as _;
use workspace::{MultiWorkspace, SaveIntent, Toast, Workspace, notifications::NotificationId};

actions!(
    agent_chat,
    [
        /// Toggles focus on the agent panel.
        ToggleFocus,
        /// Starts a new Claude Code chat.
        NewClaudeChat,
        /// Starts a new Codex chat.
        NewCodexChat,
        /// Starts a new OpenCode chat.
        NewOpencodeChat,
        /// Starts a new Antigravity chat.
        NewAntigravityChat,
        /// Continues a chat started in Claude Code, Codex, OpenCode or Antigravity.
        ContinueSavedChat,
        /// Sends the message in the chat box.
        Send,
        /// Stops the agent's current turn.
        Stop,
        /// Answers the agent's question and moves to the next one, or submits on the last.
        SubmitAnswer,
        /// Highlights the previous command in the slash command menu.
        SelectPreviousCommand,
        /// Highlights the next command in the slash command menu.
        SelectNextCommand,
        /// Picks the highlighted command in the slash command menu.
        AcceptCommand,
        /// Closes the slash command menu.
        DismissCommands,
        /// Fills the empty chat box with the prompt the agent suggests.
        AcceptPromptSuggestion,
        /// Starts or stops dictation in the chat box.
        ToggleDictation,
        /// Renames the chat from its tab.
        RenameChat,
        /// Marks the chat as unread, so its tab and list row show that it has news.
        MarkChatUnread,
        /// Expands or collapses every thought process in the chat.
        ToggleThinkingBlocks,
        /// Hides or shows the tool calls and other activity between your prompts and the agent's replies.
        ToggleFocusView,
        /// Shows or hides the chat panel.
        ToggleChatPanel,
        /// Moves the chat out of its tab and into the chat panel.
        MoveChatToPanel,
        /// Moves a chat from the chat panel back into a tab.
        MoveChatToTab,
        /// Moves focus from the chat box to the editor, or from the editor back to the chat box of the chat you last used. On Windows, Ctrl-Escape opens the Start menu, so Ctrl-Alt-Escape does this as well.
        FocusInput,
        /// Starts a new Claude Code chat in a tab, even when new chats open in the chat panel.
        NewChatInTab,
        /// Reopens the chat whose tab you closed last.
        ReopenClosedChat,
        /// Scrolls the chat to its last message and moves focus to the chat box.
        FocusLastMessage,
        /// Accepts the agent's proposed file change shown in the review tab.
        AcceptProposedChanges,
        /// Rejects the agent's proposed file change shown in the review tab.
        RejectProposedChanges,
        /// Accepts the single change under the cursor in the review tab.
        AcceptChangeAtCursor,
        /// Rejects the single change under the cursor in the review tab, restoring the file's current text there.
        RejectChangeAtCursor,
    ]
);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentKind {
    Claude,
    Codex,
    Opencode,
    Antigravity,
}

impl AgentKind {
    pub const ALL: [AgentKind; 4] = [
        AgentKind::Claude,
        AgentKind::Codex,
        AgentKind::Opencode,
        AgentKind::Antigravity,
    ];

    pub fn label(self) -> &'static str {
        match self {
            AgentKind::Claude => "Claude Code",
            AgentKind::Codex => "Codex",
            AgentKind::Opencode => "OpenCode",
            AgentKind::Antigravity => "Antigravity",
        }
    }

    pub(crate) fn has_plan_usage(self) -> bool {
        match self {
            AgentKind::Claude | AgentKind::Codex => true,
            AgentKind::Opencode | AgentKind::Antigravity => false,
        }
    }
}

/// `shell_env_loaded` resolves once the login shell's PATH is applied, so agent CLIs can be found.
pub fn init(shell_env_loaded: Option<futures::channel::oneshot::Receiver<()>>, cx: &mut App) {
    if let Some(loaded) = shell_env_loaded {
        cx.set_global(session::ShellEnvLoaded(loaded.shared()));
    }
    cx.set_global(workspace::QuitWarning(session::quit_warning));
    let changes = cx.new(|_| workspace::AgentActivityChanges);
    cx.set_global(workspace::ProjectAgentActivity {
        activity: session::project_agent_activity,
        changes,
    });
    agent_panel::init(cx);
    chat_panel::init(cx);
    cx.observe_new(|workspace: &mut Workspace, _, _| {
        workspace.register_action(
            |workspace, _: &wu_actions::agent::InsertMention, window, cx| {
                insert_mention(workspace, window, cx)
            },
        );
        workspace.register_action(
            |workspace, action: &wu_actions::agent::AddToChat, window, cx| {
                add_to_chat(workspace, action.paths.clone(), "", window, cx)
            },
        );
        workspace.register_action(
            |workspace, action: &wu_actions::agent::ResolveMergeConflicts, window, cx| {
                resolve_merge_conflicts(workspace, action.paths.clone(), window, cx)
            },
        );
    })
    .detach();
    cx.observe_new(|workspace: &mut Workspace, _, _| {
        workspace.register_action(|workspace, _: &FocusInput, window, cx| {
            toggle_input_focus(workspace, window, cx)
        });
    })
    .detach();
    offer_fix_actions(cx);
    ide_server::init(cx);
    watch_enabled(cx);
}

fn offer_fix_actions(cx: &mut App) {
    cx.observe_new(|editor: &mut editor::Editor, window, cx| {
        if let Some(window) = window
            && editor.mode().is_full()
            && editor.project().is_some()
        {
            let provider = std::rc::Rc::new(chat_view::FixWithClaude);
            editor.add_code_action_provider(provider, window, cx);
        }
    })
    .detach();
}

fn insert_mention(workspace: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
    if !agent_panel::agent_chat_enabled(cx) {
        return;
    }
    if ide_server::mention_in_terminal(workspace, window, cx) {
        return;
    }
    let Some((path, lines)) = chat_view::mention_target(workspace, cx) else {
        return;
    };
    mention_in_last_chat(workspace, &path, lines, window, cx);
}

/// The chat that last had focus per workspace. Pane activation timestamps can't tell a tab from a docked chat since the chat panel's pane counts separately.
#[derive(Default)]
struct LastFocusedChats(collections::HashMap<EntityId, WeakEntity<ChatView>>);

impl Global for LastFocusedChats {}

pub(crate) fn remember_focused_chat(
    workspace: EntityId,
    chat: WeakEntity<ChatView>,
    cx: &mut App,
) {
    cx.default_global::<LastFocusedChats>()
        .0
        .insert(workspace, chat);
}

/// Every chat of the workspace, whether open as a tab or docked in the chat panel, except subagent views.
pub(crate) fn open_chats(workspace: &Workspace, cx: &App) -> Vec<Entity<ChatView>> {
    let panel_chats = workspace
        .panel::<ChatPanel>(cx)
        .map(|panel| {
            let pane = panel.read(cx).pane().read(cx);
            pane.items_of_type::<ChatView>().collect::<Vec<_>>()
        })
        .unwrap_or_default();
    workspace
        .items_of_type::<ChatView>(cx)
        .chain(panel_chats)
        .filter(|chat| !chat.read(cx).is_subagent())
        .collect()
}

/// The chat you last focused, whether open as a tab or docked in the chat panel, else any open chat.
fn last_focused_chat(workspace: &Workspace, cx: &App) -> Option<Entity<ChatView>> {
    let open = open_chats(workspace, cx);
    let remembered = cx
        .try_global::<LastFocusedChats>()
        .and_then(|chats| chats.0.get(&workspace.weak_handle().entity_id()))
        .and_then(|chat| chat.upgrade())
        .filter(|chat| open.contains(chat));
    remembered.or_else(|| open.into_iter().next())
}

fn toggle_input_focus(workspace: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
    if !agent_panel::agent_chat_enabled(cx) {
        return;
    }
    let in_chat = open_chats(workspace, cx)
        .iter()
        .any(|chat| gpui::Focusable::focus_handle(chat.read(cx), cx).contains_focused(window, cx));
    if !in_chat {
        if let Some(chat) = reveal_last_chat(workspace, window, cx) {
            window.focus(&gpui::Focusable::focus_handle(chat.read(cx), cx), cx);
        }
        return;
    }
    let mut panes = vec![workspace.active_pane().clone()];
    panes.extend(workspace.panes().iter().cloned());
    let editor = panes.iter().find_map(|pane| {
        let pane = pane.read(cx);
        pane.active_item()
            .into_iter()
            .chain(pane.items().cloned())
            .find(|item| item.act_as::<ChatView>(cx).is_none())
    });
    if let Some(editor) = editor {
        workspace.activate_item(editor.as_ref(), true, true, window, cx);
    }
}

fn mention_in_last_chat(
    workspace: &mut Workspace,
    path: &Path,
    lines: Option<(u32, u32)>,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let Some(chat) = reveal_last_chat(workspace, window, cx) else {
        return;
    };
    chat.update(cx, |chat, cx| {
        chat.insert_file_mention(path, lines, window, cx)
    });
}

fn resolve_merge_conflicts(
    workspace: &mut Workspace,
    paths: Vec<PathBuf>,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    add_to_chat(
        workspace,
        paths,
        "Resolve the merge conflicts in ",
        window,
        cx,
    )
}

fn add_to_chat(
    workspace: &mut Workspace,
    paths: Vec<PathBuf>,
    prefix: &str,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    if !agent_panel::agent_chat_enabled(cx) || paths.is_empty() {
        return;
    }
    let Some(chat) = reveal_last_chat(workspace, window, cx) else {
        return;
    };
    chat.update(cx, |chat, cx| {
        chat.insert_mentions(paths, prefix, window, cx)
    });
}

/// Longest prompt a `wu://agent/open` link may carry, in characters.
pub const MAX_LINK_PROMPT_CHARS: usize = 10_000;

/// Whether a `wu://agent/open` link's session id is safe to look up: Wu's and Claude's ids are UUIDs, and the id names a transcript file, so anything that could leave that folder is refused.
pub fn link_session_is_valid(session: &str) -> bool {
    (1..=200).contains(&session.len())
        && session
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
}

/// Whether a `wu://agent/open` link's prompt may go in the chat box: at most `MAX_LINK_PROMPT_CHARS` characters, and no control characters but newlines and tabs, which could hide text from the user.
pub fn link_prompt_is_valid(prompt: &str) -> bool {
    prompt.chars().count() <= MAX_LINK_PROMPT_CHARS
        && prompt
            .chars()
            .all(|character| !character.is_control() || matches!(character, '\n' | '\t'))
}

/// Opens the chat a `wu://agent/open` link names, or a new Claude chat, with the link's prompt waiting in the chat box. Nothing is sent.
pub fn open_chat_from_link(
    workspace: &mut Workspace,
    session: Option<String>,
    prompt: Option<String>,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    if !agent_panel::agent_chat_enabled(cx) {
        log::info!("ignored a wu://agent/open link because Agent Chat is off");
        return;
    }
    if prompt
        .as_deref()
        .is_some_and(|prompt| !link_prompt_is_valid(prompt))
    {
        log::error!("ignored a wu://agent/open link with an invalid prompt");
        return;
    }
    let Some(session) = session else {
        if let Some(chat) = reveal_chat_or_new_claude(workspace, None, window, cx)
            && let Some(prompt) = prompt
        {
            chat.update(cx, |chat, cx| chat.place_link_prompt(&prompt, window, cx));
        }
        return;
    };
    if !link_session_is_valid(&session) {
        log::error!("ignored a wu://agent/open link with an invalid session id");
        return;
    }
    let cwd = agent_panel::chat_roots(workspace.project(), cx)
        .into_iter()
        .next()
        .unwrap_or_else(|| paths::home_dir().clone());
    let store = agent_panel::store_for(workspace, cx);
    let opening = store.update(cx, |store, cx| store.open_linked_session(session, cwd, cx));
    cx.spawn_in(window, async move |workspace, cx| {
        let result = opening.await;
        workspace.update_in(cx, |workspace, window, cx| match result {
            Ok(session) => {
                agent_panel::open_chat(workspace, session.clone(), window, cx);
                if let Some(prompt) = prompt
                    && let Some(chat) = agent_panel::chat_view_for(workspace, &session, cx)
                {
                    chat.update(cx, |chat, cx| chat.place_link_prompt(&prompt, window, cx));
                }
            }
            Err(error) => workspace.show_toast(
                Toast::new(
                    NotificationId::unique::<ChatView>(),
                    t!(
                        "Couldn't open the chat: {error}",
                        error = format!("{error:#}")
                    )
                    .to_string(),
                ),
                cx,
            ),
        })
    })
    .detach_and_log_err(cx);
}

/// Shows the Plugins dialog with a `wu://agent/install-plugin` link's plugin in the install form, which waits for the user to confirm.
pub fn install_plugin_from_link(
    workspace: &mut Workspace,
    plugin: String,
    marketplace: Option<String>,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    if !agent_panel::agent_chat_enabled(cx) {
        log::info!("ignored a wu://agent/install-plugin link because Agent Chat is off");
        return;
    }
    let claude_chat = last_focused_chat(workspace, cx)
        .filter(|chat| chat.read(cx).session().read(cx).kind() == AgentKind::Claude);
    let Some(chat) = reveal_chat_or_new_claude(workspace, claude_chat, window, cx) else {
        return;
    };
    let id = match marketplace {
        Some(marketplace) if !plugin.contains('@') => format!("{plugin}@{marketplace}"),
        _ => plugin,
    };
    // The dialog opens through the workspace, which is still being updated here.
    window.defer(cx, move |window, cx| {
        chat.update(cx, |chat, cx| {
            chat.show_plugins_dialog(Some(id), window, cx)
        })
    });
}

fn reveal_last_chat(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> Option<Entity<ChatView>> {
    let chat = last_focused_chat(workspace, cx);
    reveal_chat_or_new_claude(workspace, chat, window, cx)
}

fn reveal_chat_or_new_claude(
    workspace: &mut Workspace,
    chat: Option<Entity<ChatView>>,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> Option<Entity<ChatView>> {
    let chat = match chat {
        Some(chat) => chat,
        None => {
            let session = agent_panel::create_session(workspace, AgentKind::Claude, cx)?;
            agent_panel::open_chat(workspace, session.clone(), window, cx);
            agent_panel::chat_view_for(workspace, &session, cx)?
        }
    };
    if chat.read(cx).is_docked() {
        chat_panel::reveal_chat(workspace, &chat, window, cx);
    } else {
        workspace.activate_item(&chat, true, true, window, cx);
    }
    Some(chat)
}

fn sync_claude_launch(cx: &App) {
    AgentChatSettings::get_global(cx).claude_launch.clone().set_current();
}

fn watch_enabled(cx: &mut App) {
    sync_claude_launch(cx);
    cx.observe_global::<SettingsStore>(|cx| sync_claude_launch(cx))
        .detach();
    let mut enabled = AgentChatSettings::get_global(cx).enabled;
    show_commands(enabled, cx);
    cx.observe_global::<SettingsStore>(move |cx| {
        let now = AgentChatSettings::get_global(cx).enabled;
        if now == enabled {
            return;
        }
        enabled = now;
        show_commands(now, cx);
        ide_server::set_running(now, cx);
        if now {
            session::load_saved_chats(cx);
        } else {
            turn_off(cx);
        }
    })
    .detach();
}

fn show_commands(shown: bool, cx: &mut App) {
    CommandPaletteFilter::update_global(cx, |filter, _| {
        if shown {
            filter.show_namespace("agent_chat");
        } else {
            filter.hide_namespace("agent_chat");
        }
    });
}

fn workspace_windows(cx: &App) -> Vec<WindowHandle<MultiWorkspace>> {
    cx.windows()
        .into_iter()
        .filter_map(|window| window.downcast::<MultiWorkspace>())
        .collect()
}

fn turn_off(cx: &mut App) {
    let working = session::working_agent_count(cx);
    let prompt_window = cx
        .active_window()
        .or_else(|| workspace_windows(cx).first().map(|window| (*window).into()));
    let (true, Some(prompt_window)) = (working > 0, prompt_window) else {
        close_agent_chats(cx);
        return;
    };
    let message = if working == 1 {
        t!("An agent is still working.")
    } else {
        t!("{count} agents are still working.", count = working)
    };
    let answer = prompt_window.update(cx, |_, window, cx| {
        window.prompt(
            PromptLevel::Warning,
            &message,
            Some(t!("Turning off Agent Chat stops it.").as_str()),
            &[
                PromptButton::cancel(t!("Cancel")),
                PromptButton::new(t!("Turn Off")),
            ],
            cx,
        )
    });
    match answer {
        Ok(answer) => cx
            .spawn(async move |cx| {
                if answer.await.ok() == Some(1) {
                    cx.update(close_agent_chats);
                } else {
                    cx.update(turn_back_on);
                }
            })
            .detach(),
        Err(error) => {
            log::error!("couldn't ask before turning off agent chat: {error:#}");
            close_agent_chats(cx);
        }
    }
}

fn turn_back_on(cx: &mut App) {
    settings::update_settings_file(<dyn fs::Fs>::global(cx), cx, |content, _| {
        content.agent_chat.get_or_insert_default().enabled = None;
    });
}

fn close_agent_chats(cx: &mut App) {
    session::stop_all_agents(cx);
    for window in workspace_windows(cx) {
        window
            .update(cx, |multi, window, cx| {
                let workspaces: Vec<_> = multi.workspaces().cloned().collect();
                for workspace in workspaces {
                    workspace.update(cx, |workspace, cx| {
                        if workspace
                            .active_modal::<saved_chats::SavedChatPicker>(cx)
                            .is_some()
                        {
                            workspace.hide_modal(window, cx);
                        }
                        workspace.close_panel::<AgentPanel>(window, cx);
                        let mut panes = workspace.panes().to_vec();
                        if let Some(panel) = workspace.panel::<ChatPanel>(cx) {
                            panes.push(panel.read(cx).pane().clone());
                        }
                        for pane in panes {
                            pane.update(cx, |pane, cx| {
                                let chats: collections::HashSet<EntityId> = pane
                                    .items_of_type::<ChatView>()
                                    .map(|chat| chat.entity_id())
                                    .collect();
                                if !chats.is_empty() {
                                    pane.close_items(window, cx, SaveIntent::Skip, &|id| {
                                        chats.contains(&id)
                                    })
                                    .detach_and_log_err(cx);
                                }
                            });
                        }
                    });
                }
            })
            .log_err();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::tests::{make_global, new_store};
    use gpui::{TestAppContext, UpdateGlobal as _};
    use project::FakeFs;

    fn set_enabled(enabled: bool, cx: &mut gpui::VisualTestContext) {
        cx.update(|_, cx| {
            SettingsStore::update_global(cx, |store, cx| {
                store.update_user_settings(cx, |content| {
                    content.agent_chat.get_or_insert_default().enabled = Some(enabled);
                })
            })
        });
        cx.run_until_parked();
    }

    #[gpui::test]
    async fn turning_agent_chat_off_closes_chats_and_hides_its_commands(cx: &mut TestAppContext) {
        cx.update(|cx| {
            workspace::AppState::test(cx);
            editor::init(cx);
            command_palette_hooks::init(cx);
        });
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = new_store(directory.path(), cx);
        store.update(cx, |store, _| store.skip_plan_usage());
        cx.update(watch_enabled);
        let project = project::Project::test(FakeFs::new(cx.executor()), [], cx).await;
        let session = store.update(cx, |store, cx| {
            store.create_session(AgentKind::Claude, directory.path().to_path_buf(), cx)
        });
        let (multi_workspace, cx) = cx.add_window_view(|window, cx| {
            workspace::MultiWorkspace::test_new(project.clone(), window, cx)
        });
        let workspace = multi_workspace.read_with(cx, |multi, _| multi.workspace().clone());
        workspace.update_in(cx, |workspace, window, cx| {
            let weak = cx.entity().downgrade();
            let chat = cx.new(|cx| {
                ChatView::new(
                    session.clone(),
                    store.clone(),
                    project.clone(),
                    weak,
                    window,
                    cx,
                )
            });
            workspace.add_item_to_active_pane(Box::new(chat), None, true, window, cx);
        });
        let chat_count = |cx: &mut gpui::VisualTestContext| {
            workspace.read_with(cx, |workspace, cx| {
                workspace.items_of_type::<ChatView>(cx).count()
            })
        };
        let hidden = |cx: &mut gpui::VisualTestContext| {
            cx.update(|_, cx| {
                CommandPaletteFilter::try_global(cx)
                    .is_some_and(|filter| filter.is_hidden(&NewClaudeChat))
            })
        };
        assert_eq!(chat_count(cx), 1);
        assert!(!hidden(cx));

        set_enabled(false, cx);
        assert_eq!(chat_count(cx), 0);
        assert!(hidden(cx));

        set_enabled(true, cx);
        assert!(!hidden(cx));
    }

    #[gpui::test]
    async fn insert_mention_targets_the_last_focused_chat_and_opens_the_panel_when_none(
        cx: &mut TestAppContext,
    ) {
        cx.update(|cx| {
            workspace::AppState::test(cx);
            editor::init(cx);
        });
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = new_store(directory.path(), cx);
        store.update(cx, |store, _| store.skip_plan_usage());
        let project = project::Project::test(FakeFs::new(cx.executor()), [], cx).await;
        let (multi_workspace, cx) = cx.add_window_view(|window, cx| {
            workspace::MultiWorkspace::test_new(project.clone(), window, cx)
        });
        let workspace = multi_workspace.read_with(cx, |multi, _| multi.workspace().clone());
        let none = workspace.read_with(cx, |workspace, cx| last_focused_chat(workspace, cx));
        assert!(none.is_none(), "no chat yet, so a new one is opened");

        let mut chats = Vec::new();
        for _ in 0..2 {
            let session = store.update(cx, |store, cx| {
                store.create_session(AgentKind::Claude, directory.path().to_path_buf(), cx)
            });
            let chat = workspace.update_in(cx, |workspace, window, cx| {
                let weak = cx.entity().downgrade();
                let chat = cx.new(|cx| {
                    ChatView::new(session, store.clone(), project.clone(), weak, window, cx)
                });
                workspace.add_item_to_active_pane(Box::new(chat.clone()), None, true, window, cx);
                chat
            });
            chats.push(chat);
        }
        let focus = |chat: &Entity<ChatView>, cx: &mut gpui::VisualTestContext| {
            cx.update(|window, cx| {
                window.focus(&gpui::Focusable::focus_handle(chat.read(cx), cx), cx)
            });
            cx.run_until_parked();
        };
        focus(&chats[1], cx);
        focus(&chats[0], cx);
        let path = directory.path().join("src").join("a.rs");
        let mention = |cx: &mut gpui::VisualTestContext| {
            workspace.update_in(cx, |workspace, window, cx| {
                mention_in_last_chat(workspace, &path, Some((5, 10)), window, cx)
            });
        };
        let composer_text = |chat: &Entity<ChatView>, cx: &mut gpui::VisualTestContext| {
            chat.read_with(cx, |chat, cx| chat.composer_text_for_test(cx))
        };
        mention(cx);
        assert_eq!(composer_text(&chats[0], cx), "@src/a.rs#5-10 ");
        assert_eq!(composer_text(&chats[1], cx), "");
        mention(cx);
        assert_eq!(
            composer_text(&chats[0], cx),
            "@src/a.rs#5-10 @src/a.rs#5-10 "
        );
    }

    fn settle(
        cx: &mut gpui::VisualTestContext,
        mut done: impl FnMut(&mut gpui::VisualTestContext) -> bool,
    ) {
        for _ in 0..500 {
            cx.run_until_parked();
            if done(cx) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("the link never reached the expected state");
    }

    #[gpui::test]
    async fn open_link_validates_session_and_never_sends(cx: &mut TestAppContext) {
        cx.update(|cx| {
            workspace::AppState::test(cx);
            editor::init(cx);
        });
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = new_store(directory.path(), cx);
        store.update(cx, |store, _| store.skip_plan_usage());
        make_global(&store, cx);
        let project = project::Project::test(FakeFs::new(cx.executor()), [], cx).await;
        let (multi_workspace, cx) = cx.add_window_view(|window, cx| {
            workspace::MultiWorkspace::test_new(project.clone(), window, cx)
        });
        let workspace = multi_workspace.read_with(cx, |multi, _| multi.workspace().clone());
        let chats = |cx: &mut gpui::VisualTestContext| {
            workspace.read_with(cx, |workspace, cx| open_chats(workspace, cx))
        };
        let open_link =
            |session: Option<String>, prompt: &str, cx: &mut gpui::VisualTestContext| {
                workspace.update_in(cx, |workspace, window, cx| {
                    open_chat_from_link(workspace, session, Some(prompt.to_string()), window, cx)
                });
                cx.run_until_parked();
            };

        for session in [
            "a".repeat(201),
            "../chat".into(),
            "a\\b".into(),
            "a..b".into(),
            "a\0b".into(),
        ] {
            assert!(!link_session_is_valid(&session), "{session:?}");
            open_link(Some(session.clone()), "hi", cx);
            assert!(chats(cx).is_empty(), "{session:?} must open nothing");
        }

        let session = store.update(cx, |store, cx| {
            store.create_session(AgentKind::Claude, directory.path().to_path_buf(), cx)
        });
        let id = session.read_with(cx, |session, _| session.metadata().id.clone());
        assert!(link_session_is_valid(&id));
        open_link(Some(id), "/clear", cx);
        settle(cx, |cx| !chats(cx).is_empty());
        let chat = chats(cx).remove(0);
        assert_eq!(
            chat.read_with(cx, |chat, _| chat.session().clone()),
            session
        );
        chat.read_with(cx, |chat, cx| {
            assert_eq!(chat.composer_text_for_test(cx), "/clear");
            assert!(
                !chat.command_menu_is_open_for_test(),
                "the link's command waits as text"
            );
        });
        session.read_with(cx, |session, _| {
            assert!(
                !session.is_working() && !session.has_live_run(),
                "nothing is sent"
            );
        });

        open_link(None, "review my changes", cx);
        settle(cx, |cx| chats(cx).len() == 2);
        let new_chat = chats(cx)
            .into_iter()
            .find(|other| *other != chat)
            .expect("a new chat");
        new_chat.read_with(cx, |chat, cx| {
            assert_eq!(chat.composer_text_for_test(cx), "review my changes");
            let session = chat.session().read(cx);
            assert_eq!(session.kind(), AgentKind::Claude);
            assert!(
                !session.is_working() && !session.has_live_run(),
                "nothing is sent"
            );
        });
    }

    #[gpui::test]
    async fn install_plugin_link_requires_confirmation(cx: &mut TestAppContext) {
        cx.update(|cx| {
            workspace::AppState::test(cx);
            editor::init(cx);
        });
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = new_store(directory.path(), cx);
        store.update(cx, |store, _| store.skip_plan_usage());
        make_global(&store, cx);
        let project = project::Project::test(FakeFs::new(cx.executor()), [], cx).await;
        let (multi_workspace, cx) = cx.add_window_view(|window, cx| {
            workspace::MultiWorkspace::test_new(project.clone(), window, cx)
        });
        let workspace = multi_workspace.read_with(cx, |multi, _| multi.workspace().clone());
        workspace.update_in(cx, |workspace, window, cx| {
            install_plugin_from_link(
                workspace,
                "ponytail".into(),
                Some("ponytail-marketplace".into()),
                window,
                cx,
            )
        });
        cx.run_until_parked();

        let form_text = workspace.read_with(cx, |workspace, cx| {
            workspace
                .active_modal::<chat_view::ClaudeDialog>(cx)
                .and_then(|dialog| dialog.read(cx).install_form_text_for_test(cx))
        });
        assert_eq!(
            form_text.as_deref(),
            Some("ponytail@ponytail-marketplace"),
            "the install form waits, filled in, for the user to confirm"
        );
        let chats = workspace.read_with(cx, |workspace, cx| open_chats(workspace, cx));
        assert_eq!(chats.len(), 1);
        chats[0].read_with(cx, |chat, cx| {
            let session = chat.session().read(cx);
            assert_eq!(session.kind(), AgentKind::Claude);
            assert!(!session.is_working() && !session.has_live_run());
        });
    }

    #[gpui::test]
    async fn focus_input_toggles_between_editor_and_composer(cx: &mut TestAppContext) {
        cx.update(|cx| {
            workspace::AppState::test(cx);
            editor::init(cx);
        });
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = new_store(directory.path(), cx);
        store.update(cx, |store, _| store.skip_plan_usage());
        let project = project::Project::test(FakeFs::new(cx.executor()), [], cx).await;
        let (multi_workspace, cx) = cx.add_window_view(|window, cx| {
            workspace::MultiWorkspace::test_new(project.clone(), window, cx)
        });
        let workspace = multi_workspace.read_with(cx, |multi, _| multi.workspace().clone());
        let session = store.update(cx, |store, cx| {
            store.create_session(AgentKind::Claude, directory.path().to_path_buf(), cx)
        });
        let (editor, chat) = workspace.update_in(cx, |workspace, window, cx| {
            let editor = cx.new(workspace::item::test::TestItem::new);
            workspace.add_item_to_active_pane(Box::new(editor.clone()), None, true, window, cx);
            let weak = cx.entity().downgrade();
            let chat = cx
                .new(|cx| ChatView::new(session, store.clone(), project.clone(), weak, window, cx));
            workspace.add_item_to_active_pane(Box::new(chat.clone()), None, true, window, cx);
            (editor, chat)
        });
        let toggle = |cx: &mut gpui::VisualTestContext| {
            workspace.update_in(cx, |workspace, window, cx| {
                toggle_input_focus(workspace, window, cx)
            });
            cx.run_until_parked();
        };
        let focused = |cx: &mut gpui::VisualTestContext| {
            cx.update(|window, cx| {
                let in_chat = gpui::Focusable::focus_handle(chat.read(cx), cx)
                    .contains_focused(window, cx);
                let in_editor = gpui::Focusable::focus_handle(editor.read(cx), cx)
                    .contains_focused(window, cx);
                (in_chat, in_editor)
            })
        };
        cx.update(|window, cx| {
            window.focus(&gpui::Focusable::focus_handle(chat.read(cx), cx), cx)
        });
        cx.run_until_parked();
        assert_eq!(focused(cx), (true, false));
        toggle(cx);
        assert_eq!(focused(cx), (false, true));
        toggle(cx);
        assert_eq!(focused(cx), (true, false));
    }

    #[gpui::test]
    async fn add_to_chat_inserts_mentions_for_every_path(cx: &mut TestAppContext) {
        cx.update(|cx| {
            workspace::AppState::test(cx);
            editor::init(cx);
        });
        let directory = tempfile::tempdir().expect("temporary directory");
        std::fs::create_dir(directory.path().join("dir")).expect("create dir");
        let store = new_store(directory.path(), cx);
        store.update(cx, |store, _| store.skip_plan_usage());
        let project = project::Project::test(FakeFs::new(cx.executor()), [], cx).await;
        let (multi_workspace, cx) = cx.add_window_view(|window, cx| {
            workspace::MultiWorkspace::test_new(project.clone(), window, cx)
        });
        let workspace = multi_workspace.read_with(cx, |multi, _| multi.workspace().clone());
        let session = store.update(cx, |store, cx| {
            store.create_session(AgentKind::Claude, directory.path().to_path_buf(), cx)
        });
        let chat = workspace.update_in(cx, |workspace, window, cx| {
            let weak = cx.entity().downgrade();
            let chat = cx
                .new(|cx| ChatView::new(session, store.clone(), project.clone(), weak, window, cx));
            workspace.add_item_to_active_pane(Box::new(chat.clone()), None, true, window, cx);
            chat
        });
        let paths = ["a.rs", "b.rs", "dir"]
            .map(|name| directory.path().join(name))
            .to_vec();
        workspace.update_in(cx, |workspace, window, cx| {
            add_to_chat(workspace, paths, "", window, cx)
        });
        assert_eq!(
            chat.read_with(cx, |chat, cx| chat.composer_text_for_test(cx)),
            "@a.rs @b.rs @dir/ "
        );
    }

    #[gpui::test]
    async fn resolve_conflicts_prefills_mentions(cx: &mut TestAppContext) {
        cx.update(|cx| {
            workspace::AppState::test(cx);
            editor::init(cx);
        });
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = new_store(directory.path(), cx);
        store.update(cx, |store, _| store.skip_plan_usage());
        let project = project::Project::test(FakeFs::new(cx.executor()), [], cx).await;
        let (multi_workspace, cx) = cx.add_window_view(|window, cx| {
            workspace::MultiWorkspace::test_new(project.clone(), window, cx)
        });
        let workspace = multi_workspace.read_with(cx, |multi, _| multi.workspace().clone());
        let session = store.update(cx, |store, cx| {
            store.create_session(AgentKind::Claude, directory.path().to_path_buf(), cx)
        });
        let chat = workspace.update_in(cx, |workspace, window, cx| {
            let weak = cx.entity().downgrade();
            let chat = cx
                .new(|cx| ChatView::new(session, store.clone(), project.clone(), weak, window, cx));
            workspace.add_item_to_active_pane(Box::new(chat.clone()), None, true, window, cx);
            chat
        });
        let paths = ["a.ts", "dir/b.ts"]
            .map(|name| directory.path().join(name))
            .to_vec();
        workspace.update_in(cx, |workspace, window, cx| {
            resolve_merge_conflicts(workspace, paths, window, cx)
        });
        assert_eq!(
            chat.read_with(cx, |chat, cx| chat.composer_text_for_test(cx)),
            "Resolve the merge conflicts in @a.ts @dir/b.ts "
        );
    }

    #[gpui::test]
    async fn resolve_conflicts_separates_from_typed_text(cx: &mut TestAppContext) {
        cx.update(|cx| {
            workspace::AppState::test(cx);
            editor::init(cx);
        });
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = new_store(directory.path(), cx);
        store.update(cx, |store, _| store.skip_plan_usage());
        let project = project::Project::test(FakeFs::new(cx.executor()), [], cx).await;
        let (multi_workspace, cx) = cx.add_window_view(|window, cx| {
            workspace::MultiWorkspace::test_new(project.clone(), window, cx)
        });
        let workspace = multi_workspace.read_with(cx, |multi, _| multi.workspace().clone());
        let session = store.update(cx, |store, cx| {
            store.create_session(AgentKind::Claude, directory.path().to_path_buf(), cx)
        });
        let chat = workspace.update_in(cx, |workspace, window, cx| {
            let weak = cx.entity().downgrade();
            let chat = cx
                .new(|cx| ChatView::new(session, store.clone(), project.clone(), weak, window, cx));
            workspace.add_item_to_active_pane(Box::new(chat.clone()), None, true, window, cx);
            chat
        });
        chat.update_in(cx, |chat, window, cx| {
            chat.set_composer_text_for_test("fix this", window, cx)
        });
        let paths = ["a.ts", "dir/b.ts"]
            .map(|name| directory.path().join(name))
            .to_vec();
        workspace.update_in(cx, |workspace, window, cx| {
            resolve_merge_conflicts(workspace, paths, window, cx)
        });
        assert_eq!(
            chat.read_with(cx, |chat, cx| chat.composer_text_for_test(cx)),
            "fix this Resolve the merge conflicts in @a.ts @dir/b.ts "
        );
    }
}
