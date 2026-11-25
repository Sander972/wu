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
use workspace::{MultiWorkspace, SaveIntent, Workspace};

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
        /// Starts or stops dictation in the chat box.
        ToggleDictation,
        /// Renames the chat from its tab.
        RenameChat,
        /// Shows or hides the chat panel.
        ToggleChatPanel,
        /// Moves the chat out of its tab and into the chat panel.
        MoveChatToPanel,
        /// Moves a chat from the chat panel back into a tab.
        MoveChatToTab,
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
    // Where a mention goes besides the chat composer (a terminal running `claude`) branches here, on what is focused.
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

/// The chat you last focused, whether open as a tab or docked in the chat panel, else any open chat.
fn last_focused_chat(workspace: &Workspace, cx: &App) -> Option<Entity<ChatView>> {
    let panel_chats = workspace
        .panel::<ChatPanel>(cx)
        .map(|panel| {
            let pane = panel.read(cx).pane().read(cx);
            pane.items_of_type::<ChatView>().collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let open: Vec<Entity<ChatView>> = workspace
        .items_of_type::<ChatView>(cx)
        .chain(panel_chats)
        .filter(|chat| !chat.read(cx).is_subagent())
        .collect();
    let remembered = cx
        .try_global::<LastFocusedChats>()
        .and_then(|chats| chats.0.get(&workspace.weak_handle().entity_id()))
        .and_then(|chat| chat.upgrade())
        .filter(|chat| open.contains(chat));
    remembered.or_else(|| open.into_iter().next())
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

fn reveal_last_chat(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> Option<Entity<ChatView>> {
    let chat = match last_focused_chat(workspace, cx) {
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

fn watch_enabled(cx: &mut App) {
    let mut enabled = AgentChatSettings::get_global(cx).enabled;
    show_commands(enabled, cx);
    cx.observe_global::<SettingsStore>(move |cx| {
        let now = AgentChatSettings::get_global(cx).enabled;
        if now == enabled {
            return;
        }
        enabled = now;
        show_commands(now, cx);
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
    use crate::session::tests::new_store;
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
