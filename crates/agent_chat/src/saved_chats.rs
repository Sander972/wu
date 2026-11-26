use std::{path::PathBuf, sync::Arc};

use agent_harness::{ExternalSession, HarnessError};
use gpui::{
    AnyElement, App, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable, Render,
    SharedString, Task, WeakEntity, Window,
};
use i18n::t;
use picker::{Picker, PickerDelegate};
use settings::Settings as _;
use time::OffsetDateTime;
use ui::{ListItem, ListItemSpacing, ToggleButtonGroup, ToggleButtonSimple, prelude::*};
use util::ResultExt as _;
use workspace::{ModalView, Toast, Workspace, notifications::NotificationId};

use crate::{
    AgentChatSettings, AgentKind,
    agent_panel::{chat_roots, format_time_ago, open_chat, store_for},
    model_picker::agent_icon,
    session::AgentStore,
};

pub(crate) fn toggle(workspace: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
    let project = workspace.project().clone();
    if project.read(cx).is_via_remote_server() {
        workspace.show_toast(
            Toast::new(
                NotificationId::unique::<SavedChatPicker>(),
                t!("Agent chats only work in local projects for now.").to_string(),
            )
            .autohide(),
            cx,
        );
        return;
    }
    let roots = chat_roots(&project, cx);
    let store = store_for(workspace, cx);
    let weak_workspace = cx.entity().downgrade();
    workspace.toggle_modal(window, cx, move |window, cx| {
        SavedChatPicker::new(store, roots, weak_workspace, window, cx)
    });
}

pub(crate) struct SavedChatPicker {
    picker: Entity<Picker<SavedChatDelegate>>,
}

impl SavedChatPicker {
    fn new(
        store: Entity<AgentStore>,
        roots: Vec<PathBuf>,
        workspace: WeakEntity<Workspace>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let agents = AgentChatSettings::get_global(cx).agents().to_vec();
        let shows_web = agents.contains(&AgentKind::Claude)
            && store.update(cx, |store, _| {
                store.harness(AgentKind::Claude).shows_cloud_sessions()
            });
        let delegate = SavedChatDelegate {
            modal: cx.entity().downgrade(),
            workspace,
            store: store.clone(),
            shows_folders: roots.len() > 1,
            roots: roots.clone(),
            chats: Vec::new(),
            web_chats: Vec::new(),
            tab: Tab::Local,
            shows_web,
            web_requested: false,
            web_message: None,
            matches: Vec::new(),
            selected_index: 0,
            query: String::new(),
            pending: roots.len() * agents.len(),
            web_pending: 0,
            now: OffsetDateTime::now_utc().unix_timestamp(),
            _loads: Vec::new(),
        };
        let picker = cx.new(|cx| {
            let mut picker = Picker::uniform_list(delegate, window, cx);
            for root in roots {
                for kind in agents.iter().copied() {
                    let listing = store.update(cx, |store, cx| {
                        store.external_sessions(kind, root.clone(), cx)
                    });
                    let root = root.clone();
                    let load = cx.spawn_in(window, async move |picker, cx| {
                        let result = listing.await;
                        picker
                            .update_in(cx, |picker, window, cx| {
                                let delegate = &mut picker.delegate;
                                delegate.pending = delegate.pending.saturating_sub(1);
                                match result {
                                    Ok(sessions) => {
                                        delegate.chats.extend(sessions.into_iter().map(
                                            |session| SavedChat {
                                                kind,
                                                root: root.clone(),
                                                session,
                                            },
                                        ));
                                        delegate.chats.sort_by_key(|chat| {
                                            std::cmp::Reverse(chat.session.updated_at)
                                        });
                                    }
                                    Err(error) => {
                                        log::info!(
                                            "{} saved chats unavailable: {error:#}",
                                            kind.label()
                                        )
                                    }
                                }
                                picker.refresh(window, cx);
                            })
                            .log_err();
                    });
                    picker.delegate._loads.push(load);
                }
            }
            picker
        });
        Self { picker }
    }
}

/// Like the VS Code extension, cloud chats are only read when the user asks for them by choosing the Web tab.
fn select_tab(
    picker: &mut Picker<SavedChatDelegate>,
    tab: Tab,
    window: &mut Window,
    cx: &mut Context<Picker<SavedChatDelegate>>,
) {
    let delegate = &mut picker.delegate;
    delegate.tab = tab;
    delegate.selected_index = 0;
    if tab == Tab::Web && !delegate.web_requested {
        delegate.web_requested = true;
        for root in delegate.roots.clone() {
            delegate.web_pending += 1;
            let listing = delegate.store.update(cx, |store, cx| {
                store.cloud_sessions(AgentKind::Claude, root.clone(), cx)
            });
            let load = cx.spawn_in(window, async move |picker, cx| {
                let result = listing.await;
                picker
                    .update_in(cx, |picker, window, cx| {
                        let delegate = &mut picker.delegate;
                        delegate.web_pending = delegate.web_pending.saturating_sub(1);
                        match result {
                            Ok(None) => {
                                delegate.web_message = Some(t!(
                                    "Web chats need a recent Claude Code signed in with a Claude account."
                                ))
                            }
                            Ok(Some(sessions)) => {
                                for session in sessions {
                                    if !delegate
                                        .web_chats
                                        .iter()
                                        .any(|chat| chat.session.id == session.id)
                                    {
                                        delegate.web_chats.push(SavedChat {
                                            kind: AgentKind::Claude,
                                            root: root.clone(),
                                            session,
                                        });
                                    }
                                }
                                delegate
                                    .web_chats
                                    .sort_by_key(|chat| std::cmp::Reverse(chat.session.updated_at));
                            }
                            Err(error) => {
                                log::info!("Claude cloud chats unavailable: {error:#}");
                                delegate.web_message = Some(cloud_error_message(&error));
                            }
                        }
                        picker.refresh(window, cx);
                    })
                    .log_err();
            });
            delegate._loads.push(load);
        }
    }
    picker.refresh(window, cx);
}

/// Claude Code's refusals are sentences written for people, so they are shown without the protocol error prefix.
fn cloud_error_message(error: &anyhow::Error) -> SharedString {
    match error.downcast_ref::<HarnessError>() {
        Some(HarnessError::Protocol(detail)) => {
            i18n::lookup(detail).unwrap_or_else(|| detail.clone().into())
        }
        _ => format!("{error:#}").into(),
    }
}

impl Render for SavedChatPicker {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        v_flex().w(rems(34.)).child(self.picker.clone())
    }
}

impl Focusable for SavedChatPicker {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.picker.focus_handle(cx)
    }
}

impl EventEmitter<DismissEvent> for SavedChatPicker {}
impl ModalView for SavedChatPicker {}

#[derive(Clone, Copy, PartialEq)]
enum Tab {
    Local,
    Web,
}

#[derive(Clone)]
struct SavedChat {
    kind: AgentKind,
    root: PathBuf,
    session: ExternalSession,
}

pub(crate) struct SavedChatDelegate {
    modal: WeakEntity<SavedChatPicker>,
    workspace: WeakEntity<Workspace>,
    store: Entity<AgentStore>,
    shows_folders: bool,
    roots: Vec<PathBuf>,
    chats: Vec<SavedChat>,
    web_chats: Vec<SavedChat>,
    tab: Tab,
    /// False when Wu knows the sign-in has no cloud chats, which hides the Web tab.
    shows_web: bool,
    web_requested: bool,
    /// Why no cloud chats are listed: an error, or a sign-in or Claude Code that can't read them.
    web_message: Option<SharedString>,
    matches: Vec<usize>,
    selected_index: usize,
    query: String,
    pending: usize,
    web_pending: usize,
    now: i64,
    _loads: Vec<Task<()>>,
}

impl SavedChatDelegate {
    fn shown_chats(&self) -> &[SavedChat] {
        match self.tab {
            Tab::Local => &self.chats,
            Tab::Web => &self.web_chats,
        }
    }

    fn rematch(&mut self) {
        let query = self.query.trim().to_lowercase();
        self.matches = self
            .shown_chats()
            .iter()
            .enumerate()
            .filter(|(_, chat)| {
                query.is_empty()
                    || crate::session::display_title(&chat.session.title)
                        .to_lowercase()
                        .contains(&query)
            })
            .map(|(index, _)| index)
            .collect();
        self.selected_index = self
            .selected_index
            .min(self.matches.len().saturating_sub(1));
    }
}

impl PickerDelegate for SavedChatDelegate {
    type ListItem = ListItem;

    fn name() -> &'static str {
        "saved agent chat picker"
    }

    fn placeholder_text(&self, _window: &mut Window, _cx: &mut App) -> Arc<str> {
        t!("Continue a chat from Claude Code, Codex, OpenCode or Antigravity…")
            .as_str()
            .into()
    }

    fn no_matches_text(&self, _window: &mut Window, _cx: &mut App) -> Option<SharedString> {
        let (pending, empty) = match self.tab {
            Tab::Local => (self.pending, t!("No saved chats for this project")),
            Tab::Web => (self.web_pending, t!("No web chats for this repository")),
        };
        Some(if pending > 0 {
            t!("Loading chats…")
        } else if let Some(message) = self.web_message.clone().filter(|_| self.tab == Tab::Web) {
            message
        } else if self.query.trim().is_empty() {
            empty
        } else {
            t!("No matching chats")
        })
    }

    fn match_count(&self) -> usize {
        self.matches.len()
    }

    fn selected_index(&self) -> usize {
        self.selected_index
    }

    fn set_selected_index(&mut self, index: usize, _: &mut Window, _: &mut Context<Picker<Self>>) {
        self.selected_index = index;
    }

    fn update_matches(
        &mut self,
        query: String,
        _: &mut Window,
        _: &mut Context<Picker<Self>>,
    ) -> Task<()> {
        self.query = query;
        self.rematch();
        Task::ready(())
    }

    fn confirm(&mut self, _: bool, window: &mut Window, cx: &mut Context<Picker<Self>>) {
        if !AgentChatSettings::get_global(cx).enabled {
            self.dismissed(window, cx);
            return;
        }
        let Some(chat) = self
            .matches
            .get(self.selected_index)
            .and_then(|index| self.shown_chats().get(*index))
            .cloned()
        else {
            return;
        };
        let tab = self.tab;
        let opening = self.store.update(cx, |store, cx| match tab {
            Tab::Local => store.continue_external(chat.kind, chat.root, chat.session, cx),
            Tab::Web => store.continue_cloud(chat.kind, chat.root, chat.session, cx),
        });
        let workspace = self.workspace.clone();
        window
            .spawn(cx, async move |cx| match opening.await {
                Ok(session) => workspace
                    .update_in(cx, |workspace, window, cx| {
                        open_chat(workspace, session, window, cx)
                    })
                    .log_err(),
                Err(error) => workspace
                    .update(cx, |workspace, cx| {
                        workspace.show_toast(
                            Toast::new(
                                NotificationId::unique::<SavedChatPicker>(),
                                t!(
                                    "Couldn't open the chat: {error}",
                                    error = format!("{error:#}")
                                )
                                .to_string(),
                            ),
                            cx,
                        )
                    })
                    .log_err(),
            })
            .detach();
        self.dismissed(window, cx);
    }

    fn dismissed(&mut self, _: &mut Window, cx: &mut Context<Picker<Self>>) {
        self.modal
            .update(cx, |_, cx| cx.emit(DismissEvent))
            .log_err();
    }

    fn render_header(&self, _: &mut Window, cx: &mut Context<Picker<Self>>) -> Option<AnyElement> {
        if !self.shows_web {
            return None;
        }
        let tab_button = |label: SharedString, tab: Tab, cx: &mut Context<Picker<Self>>| {
            ToggleButtonSimple::new(
                label,
                cx.listener(move |picker, _, window, cx| select_tab(picker, tab, window, cx)),
            )
            .selected(self.tab == tab)
        };
        let buttons = [
            tab_button(t!("Local"), Tab::Local, cx),
            tab_button(t!("Web"), Tab::Web, cx),
        ];
        Some(
            h_flex()
                .px_2()
                .pt_2()
                .child(
                    ToggleButtonGroup::single_row("saved-chat-tabs", buttons)
                        .selected_index(usize::from(self.tab == Tab::Web)),
                )
                .into_any_element(),
        )
    }

    fn render_match(
        &self,
        index: usize,
        selected: bool,
        _: &mut Window,
        _: &mut Context<Picker<Self>>,
    ) -> Option<Self::ListItem> {
        let chat = self.shown_chats().get(*self.matches.get(index)?)?;
        let folder = self
            .shows_folders
            .then(|| chat.root.file_name())
            .flatten()
            .map(|name| name.to_string_lossy().into_owned());
        Some(
            ListItem::new(index)
                .inset(true)
                .spacing(ListItemSpacing::Sparse)
                .toggle_state(selected)
                .start_slot(agent_icon(chat.kind).size(IconSize::Small))
                .child(
                    h_flex()
                        .gap_2()
                        .min_w_0()
                        .child(
                            Label::new(crate::session::display_title(&chat.session.title))
                                .truncate(),
                        )
                        .children(folder.map(|folder| {
                            Label::new(folder)
                                .size(LabelSize::Small)
                                .color(Color::Muted)
                        })),
                )
                .end_slot(
                    Label::new(format_time_ago(chat.session.updated_at, self.now))
                        .size(LabelSize::Small)
                        .color(Color::Muted),
                ),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::tests::{new_store, run_until};
    use gpui::TestAppContext;

    #[gpui::test]
    async fn web_chats_are_listed_only_once_the_web_tab_is_chosen(cx: &mut TestAppContext) {
        cx.update(|cx| {
            workspace::AppState::test(cx);
            editor::init(cx);
        });
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = new_store(directory.path(), cx);
        // The fake CLI logs each control request it gets into this file once it exists.
        let requests = directory.path().join(".cloud-requests");
        std::fs::write(&requests, "").expect("request log");
        let roots = vec![directory.path().to_path_buf()];
        let (modal, cx) = cx.add_window_view(|window, cx| {
            SavedChatPicker::new(store, roots, WeakEntity::new_invalid(), window, cx)
        });
        cx.run_until_parked();
        let picker = modal.read_with(cx, |modal, _| modal.picker.clone());
        picker.read_with(cx, |picker, _| assert!(picker.delegate.shows_web));
        assert_eq!(
            std::fs::read_to_string(&requests).expect("request log"),
            "",
            "opening the picker reads no cloud chats"
        );

        picker.update_in(cx, |picker, window, cx| {
            select_tab(picker, Tab::Web, window, cx)
        });
        run_until(cx, |cx| {
            picker.read_with(cx, |picker, _| picker.delegate.web_pending == 0)
        });

        picker.read_with(cx, |picker, _| {
            assert_eq!(picker.delegate.web_message, None);
            assert_eq!(picker.delegate.matches.len(), 3);
        });
        let sent: Vec<serde_json::Value> = std::fs::read_to_string(&requests)
            .expect("request log")
            .lines()
            .map(|line| {
                serde_json::from_str::<serde_json::Value>(line).expect("json line")["request"]
                    .clone()
            })
            .collect();
        assert_eq!(
            sent,
            [serde_json::json!({"subtype": "list_cloud_sessions", "user_initiated": true})]
        );
    }
}
