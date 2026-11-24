use git_ui::git_panel::GitPanel;
use gpui::{
    Action as _, App, Context, Entity, IntoElement, ParentElement, Render, Subscription,
    WeakEntity, Window,
};
use i18n::t;
use project::{Project, git_store::GitStoreEvent};
use settings::Settings as _;
use title_bar::TitleBar;
use ui::{Tooltip, prelude::*};
use workspace::{HideStatusItem, StatusBarSettings, StatusItemView, Workspace, item::ItemHandle};

/// Worktree and branch pickers plus the Fetch/Push/Pull button, shown in the status bar.
pub struct GitStatusItem {
    workspace: WeakEntity<Workspace>,
    _subscriptions: Vec<Subscription>,
}

impl GitStatusItem {
    pub fn new(workspace: &Workspace, cx: &mut Context<Self>) -> Self {
        let git_store = workspace.project().read(cx).git_store().clone();
        let mut subscriptions = vec![cx.subscribe(&git_store, |_, _, event, cx| match event {
            GitStoreEvent::ActiveRepositoryChanged(_)
            | GitStoreEvent::RepositoryUpdated(_, _, true) => cx.notify(),
            _ => {}
        })];
        if let Some(workspace_entity) = workspace.weak_handle().upgrade() {
            subscriptions.push(cx.subscribe(
                &workspace_entity,
                |this, _, event: &workspace::Event, cx| match event {
                    workspace::Event::WorktreeCreationChanged => cx.notify(),
                    // The git panel loads asynchronously; observe it so remote operation progress re-renders the button.
                    workspace::Event::PanelAdded(panel) => {
                        if let Ok(git_panel) = panel.clone().downcast::<GitPanel>() {
                            this._subscriptions
                                .push(cx.observe(&git_panel, |_, _, cx| cx.notify()));
                            cx.notify();
                        }
                    }
                    _ => {}
                },
            ));
        }
        Self {
            workspace: workspace.weak_handle(),
            _subscriptions: subscriptions,
        }
    }
}

impl Render for GitStatusItem {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let status_bar_settings = StatusBarSettings::get_global(cx);
        if !status_bar_settings.git_controls {
            return h_flex();
        }
        let show_worktree = status_bar_settings.worktree_button;
        let Some(workspace) = self.workspace.upgrade() else {
            return h_flex();
        };
        let title_bar = workspace
            .read(cx)
            .titlebar_item()
            .and_then(|item| item.downcast::<TitleBar>().ok());
        let git_controls = title_bar.and_then(|title_bar| {
            title_bar.update(cx, |title_bar, cx| {
                title_bar.render_git_controls(show_worktree, true, gpui::Anchor::BottomLeft, cx)
            })
        });
        let Some(git_controls) = git_controls else {
            return h_flex();
        };
        let remote_button = workspace
            .read(cx)
            .panel::<GitPanel>(cx)
            .and_then(|git_panel| {
                git_panel.update(cx, |git_panel, cx| git_panel.render_remote_button(true, cx))
            });
        h_flex().gap_1().child(git_controls).children(remote_button)
    }
}

impl StatusItemView for GitStatusItem {
    fn set_active_pane_item(
        &mut self,
        _active_pane_item: Option<&dyn ItemHandle>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
    }

    fn hide_setting(&self, _: &App) -> Option<HideStatusItem> {
        Some(HideStatusItem::new(|settings| {
            settings.status_bar.get_or_insert_default().git_controls = Some(false);
        }))
    }

    fn status_bar_item(&self, _: &App) -> workspace::StatusBarItem {
        workspace::StatusBarItem::Git
    }

    fn is_visible(&self, cx: &App) -> bool {
        StatusBarSettings::get_global(cx).git_controls
    }
}

/// Opens the git graph; shown in the status bar while the project has a repository.
pub struct GitGraphButton {
    project: Entity<Project>,
    _subscription: Subscription,
}

impl GitGraphButton {
    pub fn new(workspace: &Workspace, cx: &mut Context<Self>) -> Self {
        let project = workspace.project().clone();
        let git_store = project.read(cx).git_store().clone();
        let subscription = cx.subscribe(&git_store, |_, _, event, cx| {
            if let GitStoreEvent::ActiveRepositoryChanged(_) = event {
                cx.notify();
            }
        });
        Self {
            project,
            _subscription: subscription,
        }
    }
}

impl Render for GitGraphButton {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        IconButton::new("status-bar-git-graph", IconName::GitGraph)
            .icon_size(IconSize::Small)
            .tooltip(|_window, cx| {
                Tooltip::for_action(t!("Open Git Graph"), &git_ui::git_graph::Open, cx)
            })
            .on_click(|_, window, cx| {
                window.dispatch_action(git_ui::git_graph::Open.boxed_clone(), cx)
            })
    }
}

impl StatusItemView for GitGraphButton {
    fn set_active_pane_item(
        &mut self,
        _active_pane_item: Option<&dyn ItemHandle>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
    }

    fn hide_setting(&self, _: &App) -> Option<HideStatusItem> {
        Some(HideStatusItem::new(|settings| {
            settings.status_bar.get_or_insert_default().git_graph_button = Some(false);
        }))
    }

    fn status_bar_item(&self, _: &App) -> workspace::StatusBarItem {
        workspace::StatusBarItem::GitGraph
    }

    fn is_visible(&self, cx: &App) -> bool {
        StatusBarSettings::get_global(cx).git_graph_button
            && self.project.read(cx).active_repository(cx).is_some()
    }
}
