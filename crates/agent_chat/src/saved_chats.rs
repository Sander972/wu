use std::{path::PathBuf, sync::Arc};

use agent_harness::ExternalSession;
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
        let delegate = SavedChatDelegate {
            modal: cx.entity().downgrade(),
            workspace,
            store: store.clone(),
            shows_folders: roots.len() > 1,
            chats: Vec::new(),
            web_chats: Vec::new(),
            tab: Tab::Local,
            shows_web: false,
            web_error: None,
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
                if agents.contains(&AgentKind::Claude) {
                    picker.delegate.web_pending += 1;
                    let listing = store.update(cx, |store, cx| {
                        store.cloud_sessions(AgentKind::Claude, root.clone(), cx)
                    });
                    let load = cx.spawn_in(window, async move |picker, cx| {
                        let result = listing.await;
                        picker
                            .update_in(cx, |picker, window, cx| {
                                let delegate = &mut picker.delegate;
                                delegate.web_pending = delegate.web_pending.saturating_sub(1);
                                match result {
                                    Ok(None) => {}
                                    Ok(Some(sessions)) => {
                                        delegate.shows_web = true;
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
                                        delegate.web_chats.sort_by_key(|chat| {
                                            std::cmp::Reverse(chat.session.updated_at)
                                        });
                                    }
                                    Err(error) => {
                                        log::info!("Claude cloud chats unavailable: {error:#}");
                                        delegate.shows_web = true;
                                        delegate.web_error = Some(format!("{error:#}"));
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
    chats: Vec<SavedChat>,
    web_chats: Vec<SavedChat>,
    tab: Tab,
    /// Stays false for a sign-in without cloud chats, which hides the Web tab.
    shows_web: bool,
    web_error: Option<String>,
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
        } else if let Some(error) = self.web_error.as_ref().filter(|_| self.tab == Tab::Web) {
            error.clone().into()
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
                cx.listener(move |picker, _, window, cx| {
                    picker.delegate.tab = tab;
                    picker.delegate.selected_index = 0;
                    picker.refresh(window, cx);
                }),
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
