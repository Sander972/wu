use super::{ChatView, SubagentRef, attachments, links::FileTarget, plugin_ui};
use crate::chat_style::hairline;
use editor::Editor;
use gpui::{
    Animation, AnimationExt as _, AnyElement, App, Context, Entity, ExternalPaths, FocusHandle,
    Focusable, IntoElement, ParentElement, ScrollWheelEvent, SharedString, Styled, Task, Window,
    canvas, ease_out_quint, px,
};
use i18n::t;
use std::time::Duration;
use theme::ActiveTheme as _;
use ui::{Icon, IconButton, IconName, IconSize, Label, LabelSize, Tooltip, prelude::*};
use util::ResultExt as _;
use workspace::DraggedSelection;

const SLIDE_IN: Duration = Duration::from_millis(220);
const WIDTH_FRACTION: f32 = 0.5;
const MIN_WIDTH: f32 = 380.;
const FALLBACK_AVAILABLE_WIDTH: f32 = 960.;
const HEADER_HEIGHT: f32 = 36.;
const PLUGIN_PANE_PADDING: f32 = 12.;

pub(super) enum SidePaneContent {
    Subagent {
        subagent: SubagentRef,
        view: Entity<ChatView>,
    },
    File {
        target: FileTarget,
        editor: Entity<Editor>,
    },
    SideQuestions,
    /// The panes plugins opened, one tab each; which one shows is in `PluginUi`.
    PluginPanes {
        focus_handle: FocusHandle,
    },
}

/// A `/btw` question that has no answer in the session yet.
pub(super) enum PendingSideQuestion {
    Asking {
        question: String,
        // Dropping the task cancels the request and any one-off CLI process behind it.
        _request: Task<()>,
    },
    Failed {
        question: String,
        message: SharedString,
    },
}

impl SidePaneContent {
    fn focus_handle(&self, cx: &App) -> Option<FocusHandle> {
        match self {
            SidePaneContent::Subagent { view, .. } => Some(view.focus_handle(cx)),
            SidePaneContent::File { editor, .. } => Some(editor.focus_handle(cx)),
            SidePaneContent::SideQuestions => None,
            SidePaneContent::PluginPanes { focus_handle } => Some(focus_handle.clone()),
        }
    }
}

pub(super) struct SidePane {
    content: SidePaneContent,
    opening: usize,
}

impl ChatView {
    pub(super) fn side_pane_content(&self) -> Option<&SidePaneContent> {
        self.side_pane.as_ref().map(|pane| &pane.content)
    }

    fn show_in_side_pane(
        &mut self,
        content: SidePaneContent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let opening = match &self.side_pane {
            Some(pane) => pane.opening,
            None => {
                self.side_pane_openings += 1;
                self.side_pane_openings
            }
        };
        // A plugin pane takes focus only when its plugin asks for it.
        if !matches!(content, SidePaneContent::PluginPanes { .. })
            && let Some(focus_handle) = content.focus_handle(cx)
        {
            window.focus(&focus_handle, cx);
        }
        self.side_pane = Some(SidePane { content, opening });
        cx.notify();
    }

    pub(super) fn open_subagent_in_side_pane(
        &mut self,
        subagent: SubagentRef,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.docked {
            self.open_subagent_in_tab(subagent, window, cx);
            return;
        }
        if let Some(SidePaneContent::Subagent {
            subagent: open,
            view,
        }) = self.side_pane_content()
            && open.id == subagent.id
        {
            window.focus(&view.focus_handle(cx), cx);
            return;
        }
        self._side_pane_load = Task::ready(());
        let session = self.session.clone();
        let store = self.store.clone();
        let project = self.project.clone();
        let workspace = self.workspace.clone();
        let host = cx.entity().downgrade();
        let view = cx.new(|cx| {
            let mut view = ChatView::build(
                session,
                Some(subagent.clone()),
                store,
                project,
                workspace,
                window,
                cx,
            );
            view.host = Some(host);
            view
        });
        self.show_in_side_pane(SidePaneContent::Subagent { subagent, view }, window, cx);
    }

    pub(super) fn open_file_in_side_pane(
        &mut self,
        target: FileTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if attachments::is_image_path(&target.path) {
            self.open_image(target.path, window, cx);
            return;
        }
        if self.docked {
            self.open_file_in_tab(target, window, cx);
            return;
        }
        if let Some(SidePaneContent::File {
            target: open,
            editor,
        }) = self.side_pane_content()
            && open.path == target.path
        {
            let editor = editor.clone();
            editor.update(cx, |editor, cx| {
                if let Some(line) = target.line {
                    editor.go_to_singleton_buffer_point(
                        language::Point::new(line.saturating_sub(1), 0),
                        window,
                        cx,
                    );
                }
            });
            window.focus(&editor.focus_handle(cx), cx);
            self._side_pane_load = Task::ready(());
            return;
        }
        let opening = self.project.update(cx, |project, cx| {
            project.open_local_buffer(&target.path, cx)
        });
        let project = self.project.clone();
        self._side_pane_load = cx.spawn_in(window, async move |this, cx| {
            let Some(buffer) = opening.await.log_err() else {
                this.update_in(cx, |this, window, cx| {
                    this.open_file_in_tab(target, window, cx)
                })
                .log_err();
                return;
            };
            this.update_in(cx, |this, window, cx| {
                let editor = cx.new(|cx| {
                    let mut editor = Editor::for_buffer(buffer, Some(project), window, cx);
                    editor.set_read_only(true);
                    editor.set_show_code_actions(false, cx);
                    editor.set_show_runnables(false, cx);
                    editor.set_show_breakpoints(false, cx);
                    editor.set_show_bookmarks(false, cx);
                    editor.set_gutter_matches_text_background(true, cx);
                    if let Some(line) = target.line {
                        editor.go_to_singleton_buffer_point(
                            language::Point::new(line.saturating_sub(1), 0),
                            window,
                            cx,
                        );
                    }
                    editor
                });
                this.show_in_side_pane(SidePaneContent::File { target, editor }, window, cx);
            })
            .log_err();
        });
    }

    pub(super) fn ask_side_question(
        &mut self,
        question: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !matches!(
            self.side_pane_content(),
            Some(SidePaneContent::SideQuestions)
        ) {
            self.show_in_side_pane(SidePaneContent::SideQuestions, window, cx);
        }
        let Some(question) = question else {
            return;
        };
        self.side_questions_asked += 1;
        let id = self.side_questions_asked;
        self.pending_side_questions
            .retain(|(_, pending)| matches!(pending, PendingSideQuestion::Asking { .. }));
        let asked = question.clone();
        let reply = self.session.update(cx, |session, cx| {
            session.control(
                serde_json::json!({ "subtype": "side_question", "question": question }),
                cx,
            )
        });
        let request = cx.spawn(async move |this, cx| {
            let outcome = reply.await;
            this.update(cx, |this, cx| {
                let answer = outcome
                    .as_ref()
                    .ok()
                    .and_then(|reply| reply["response"].as_str());
                this.pending_side_questions
                    .retain(|(pending_id, _)| *pending_id != id);
                if let Some(answer) = answer {
                    this.session.update(cx, |session, cx| {
                        session.push_side_answer(question, answer.to_string(), cx)
                    });
                } else {
                    let message = match outcome {
                        Ok(_) => t!("The agent had no answer to that question."),
                        Err(error) => error.to_string().into(),
                    };
                    this.pending_side_questions
                        .push((id, PendingSideQuestion::Failed { question, message }));
                }
                cx.notify();
            })
            .log_err();
        });
        self.pending_side_questions.push((
            id,
            PendingSideQuestion::Asking {
                question: asked,
                _request: request,
            },
        ));
    }

    fn render_side_questions(&self, cx: &Context<Self>) -> AnyElement {
        let answers = self.session.read(cx).side_answers();
        let question_row = |question: String, index: Option<usize>| {
            h_flex()
                .gap(px(6.))
                .items_start()
                .child(
                    Label::new(question)
                        .color(Color::Muted)
                        .size(LabelSize::Small),
                )
                .children(index.map(|index| {
                    IconButton::new(("agent-side-answer-remove", index), IconName::Trash)
                        .icon_size(IconSize::Small)
                        .tooltip(Tooltip::text(t!("Remove")))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.session
                                .update(cx, |session, cx| session.remove_side_answer(index, cx))
                        }))
                }))
        };
        let pending = self
            .pending_side_questions
            .iter()
            .rev()
            .map(|(_, pending)| {
                let (question, note) = match pending {
                    PendingSideQuestion::Asking { question, .. } => (
                        question.clone(),
                        Label::new(t!("Waiting for an answer…")).color(Color::Muted),
                    ),
                    PendingSideQuestion::Failed { question, message } => (
                        question.clone(),
                        Label::new(message.clone()).color(Color::Error),
                    ),
                };
                v_flex()
                    .gap(px(4.))
                    .child(question_row(question, None))
                    .child(note)
            });
        let empty = self.pending_side_questions.is_empty() && answers.is_empty();
        v_flex()
            .id("agent-side-questions")
            .size_full()
            .overflow_y_scroll()
            .p(px(12.))
            .gap(px(16.))
            .children(pending)
            .children(
                answers
                    .iter()
                    .enumerate()
                    .rev()
                    .map(|(index, (question, answer))| {
                        v_flex()
                            .gap(px(4.))
                            .child(question_row(question.clone(), Some(index)))
                            .child(div().text_sm().child(answer.clone()))
                    }),
            )
            .when(empty, |this| {
                this.child(
                    Label::new(t!(
                        "Ask with /btw and a question. Answers stay here and are not saved with the chat."
                    ))
                    .color(Color::Muted),
                )
            })
            .into_any_element()
    }

    pub(super) fn close_side_pane(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self._side_pane_load = Task::ready(());
        let Some(pane) = self.side_pane.take() else {
            return;
        };
        if pane
            .content
            .focus_handle(cx)
            .is_some_and(|focus_handle| focus_handle.contains_focused(window, cx))
        {
            window.focus(&self.focus_handle(cx), cx);
        }
        cx.notify();
    }

    pub(super) fn move_side_pane_to_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self._side_pane_load = Task::ready(());
        let Some(pane) = self.side_pane.take() else {
            return;
        };
        match pane.content {
            SidePaneContent::Subagent { subagent, .. } => {
                self.open_subagent_in_tab(subagent, window, cx)
            }
            SidePaneContent::File { target, .. } => self.open_file_in_tab(target, window, cx),
            SidePaneContent::SideQuestions | SidePaneContent::PluginPanes { .. } => {}
        }
        cx.notify();
    }

    /// Opens the side pane for a pane a plugin just opened or showed, closes it once no pane is left, and moves focus where a plugin asked, once.
    pub(super) fn sync_plugin_panes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let showing = matches!(
            self.side_pane_content(),
            Some(SidePaneContent::PluginPanes { .. })
        );
        let reveal = std::mem::take(&mut self.plugin_ui.reveal_panes);
        if self.plugin_ui.panes.is_empty() {
            if showing {
                self.close_side_pane(window, cx);
            }
            return;
        }
        if reveal && !showing && !self.docked {
            let focus_handle = cx.focus_handle();
            self.show_in_side_pane(SidePaneContent::PluginPanes { focus_handle }, window, cx);
        }
        let Some(requested) = self.plugin_ui.focus_requested.take() else {
            return;
        };
        let focus_handle = match self.side_pane_content() {
            Some(SidePaneContent::PluginPanes { focus_handle }) => Some(focus_handle.clone()),
            _ => None,
        };
        // As the extension does, a pane doesn't take focus from a prompt being typed.
        let takes_focus = self.plugin_ui.visible_pane().map(|pane| &pane.id) == Some(&requested)
            && self.composer.read(cx).text(cx).is_empty();
        match focus_handle.filter(|_| takes_focus) {
            Some(focus_handle) => {
                window.focus(&focus_handle, cx);
                self.plugin_ui
                    .focus_pane(&self.session, Some(&requested), cx);
            }
            None => self.plugin_ui.focus_pane(&self.session, None, cx),
        }
    }

    /// Escape: a plugin pane closes only when it asked to, otherwise focus goes back to the chat.
    fn cancel_side_pane(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !matches!(
            self.side_pane_content(),
            Some(SidePaneContent::PluginPanes { .. })
        ) {
            self.close_side_pane(window, cx);
            return;
        }
        let Some(pane) = self.plugin_ui.visible_pane() else {
            return;
        };
        if pane.close_on_escape {
            self.plugin_ui.close_pane(&self.session, &pane.id, cx);
        } else {
            window.focus(&self.focus_handle(cx), cx);
            self.plugin_ui.focus_pane(&self.session, None, cx);
        }
    }

    /// The header's close button: a plugin pane is asked to close, anything else closes at once.
    pub(super) fn dismiss_side_pane(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !matches!(
            self.side_pane_content(),
            Some(SidePaneContent::PluginPanes { .. })
        ) {
            self.close_side_pane(window, cx);
            return;
        }
        if let Some(pane) = self.plugin_ui.visible_pane() {
            self.plugin_ui.close_pane(&self.session, &pane.id, cx);
        }
    }

    fn render_plugin_pane_tabs(&self, cx: &Context<Self>) -> AnyElement {
        let visible = self.plugin_ui.visible_pane().map(|pane| pane.id.clone());
        h_flex()
            .min_w_0()
            .flex_1()
            .gap(px(2.))
            .overflow_hidden()
            .children(self.plugin_ui.panes.iter().map(|pane| {
                let id = pane.id.clone();
                let selector = format!("plugin-ui-pane-tab-{id}");
                div().debug_selector(|| selector).child(
                    Button::new(
                        SharedString::from(format!("plugin-ui-pane-tab-{id}")),
                        pane.title.clone(),
                    )
                    .label_size(LabelSize::Small)
                    .toggle_state(visible.as_ref() == Some(&id))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.plugin_ui.show_pane(&this.session, &id, cx)
                    })),
                )
            }))
            .into_any_element()
    }

    fn render_plugin_pane(
        &self,
        focus_handle: &FocusHandle,
        width: f32,
        cx: &Context<Self>,
    ) -> AnyElement {
        let Some(pane) = self.plugin_ui.visible_pane() else {
            return div().into_any_element();
        };
        let rem = f32::from(self.plugin_ui.rem);
        // shortcut: cells of 0.6 by 1.4 rem, as for the attach viewport; measure them once a plugin needs exact cells.
        let (cell_width, cell_height) = (rem * 0.6, rem * 1.4);
        let height = self
            .root_bounds
            .get()
            .map_or(0., |bounds| f32::from(bounds.size.height))
            - HEADER_HEIGHT;
        let fit = |space: f32, cell: f32| {
            ((space - 2. * PLUGIN_PANE_PADDING) / cell).floor().max(1.) as u32
        };
        let (columns, rows) =
            plugin_ui::pane_cells(pane, fit(width, cell_width), fit(height, cell_height));
        let offset = self
            .plugin_ui
            .pane_offsets
            .get(&pane.id)
            .copied()
            .unwrap_or(0);
        let drawing = self
            .plugin_drawing(
                "Pane",
                &pane.id,
                || {
                    serde_json::json!({
                        "title": pane.title,
                        "isFocused": self.plugin_ui.focused_pane.as_ref() == Some(&pane.id),
                        "bodyColumns": columns,
                        "placement": "dock",
                        "scroll": { "offset": offset, "bodyRows": rows },
                        "view": {},
                    })
                },
                &mut None,
                cx,
            )
            .unwrap_or_else(|| plugin_ui::caption(std::slice::from_ref(&pane.plugin), cx));
        let id = pane.id.clone();
        let content_rows = self.plugin_ui.pane_content_rows(&id);
        v_flex()
            .id("plugin-ui-pane")
            .track_focus(focus_handle)
            .size_full()
            .p(px(PLUGIN_PANE_PADDING))
            .overflow_hidden()
            .when_some(pane.background, |this, color| this.bg(color))
            .on_scroll_wheel(cx.listener(move |this, event: &ScrollWheelEvent, _, cx| {
                let lines = event.delta.pixel_delta(px(cell_height)).y / px(cell_height);
                // shortcut: a trackpad's small deltas round to no rows; add them up once a plugin pane needs smooth scrolling.
                let by = -(lines.round() as i32);
                if by != 0 {
                    this.plugin_ui
                        .scroll_pane(&this.session, &id, by, rows, content_rows, cx);
                }
            }))
            .child(
                div()
                    .max_w(px(columns as f32 * cell_width))
                    .max_h(px(rows as f32 * cell_height))
                    .overflow_hidden()
                    .child(drawing),
            )
            .into_any_element()
    }

    pub(super) fn track_root_bounds(&self) -> impl IntoElement {
        let root_bounds = self.root_bounds.clone();
        let pane_open = self.side_pane.is_some();
        canvas(
            move |bounds, window, _| {
                let resized = root_bounds
                    .get()
                    .is_some_and(|previous| previous.size.width != bounds.size.width);
                root_bounds.set(Some(bounds));
                // The chat is laid out at the width measured last frame, so catch up after a resize.
                if pane_open && resized {
                    window.request_animation_frame();
                }
            },
            |_, _, _, _| {},
        )
        .absolute()
        .size_full()
    }

    pub(super) fn render_with_side_pane(&self, chat: AnyElement, cx: &Context<Self>) -> AnyElement {
        let Some(pane) = self.side_pane.as_ref() else {
            return chat;
        };
        let background = cx.theme().colors().editor_background;
        let available = self
            .root_bounds
            .get()
            .map_or(FALLBACK_AVAILABLE_WIDTH, |bounds| {
                f32::from(bounds.size.width)
            });
        let width = (available * WIDTH_FRACTION).max(MIN_WIDTH).min(available);
        let (icon, title, folder, body): (_, SharedString, Option<SharedString>, AnyElement) =
            match &pane.content {
                SidePaneContent::Subagent { subagent, view } => (
                    IconName::AgentBot,
                    subagent.title.clone(),
                    None,
                    view.clone().into_any_element(),
                ),
                SidePaneContent::SideQuestions => (
                    IconName::Chat,
                    t!("Side questions"),
                    None,
                    self.render_side_questions(cx),
                ),
                SidePaneContent::PluginPanes { focus_handle } => (
                    IconName::Blocks,
                    SharedString::default(),
                    None,
                    self.render_plugin_pane(focus_handle, width, cx),
                ),
                SidePaneContent::File { target, editor } => {
                    let cwd = &self.session.read(cx).metadata().cwd;
                    let shown = target.path.strip_prefix(cwd).unwrap_or(&target.path);
                    (
                        IconName::File,
                        shown
                            .file_name()
                            .map(|name| name.to_string_lossy().into_owned())
                            .unwrap_or_default()
                            .into(),
                        shown
                            .parent()
                            .filter(|parent| !parent.as_os_str().is_empty())
                            .map(|parent| parent.to_string_lossy().into_owned().into()),
                        editor.clone().into_any_element(),
                    )
                }
            };
        let tabs = matches!(pane.content, SidePaneContent::PluginPanes { .. })
            .then(|| self.render_plugin_pane_tabs(cx));
        let plugin_panes = tabs.is_some();
        let header = h_flex()
            .h(px(HEADER_HEIGHT))
            .flex_none()
            .gap(px(8.))
            .pl(px(12.))
            .pr(px(6.))
            .bg(background)
            .border_b_1()
            .border_color(hairline(0.07, cx))
            .child(Icon::new(icon).size(IconSize::Small).color(Color::Muted))
            .children(tabs)
            .when(!plugin_panes, |header| {
                header.child(
                    h_flex()
                        .min_w_0()
                        .flex_1()
                        .gap(px(6.))
                        .child(
                            Label::new(title)
                                .size(LabelSize::Small)
                                .single_line()
                                .truncate(),
                        )
                        .children(folder.map(|folder| {
                            Label::new(folder)
                                .size(LabelSize::Small)
                                .color(Color::Muted)
                                .single_line()
                                .truncate()
                        })),
                )
            })
            .when(!plugin_panes, |header| {
                header.child(
                    IconButton::new("agent-side-pane-open-in-tab", IconName::Maximize)
                        .icon_size(IconSize::Small)
                        .tooltip(Tooltip::text(t!("Open in Tab")))
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.move_side_pane_to_tab(window, cx)
                        })),
                )
            })
            .child(
                IconButton::new("agent-side-pane-close", IconName::Close)
                    .icon_size(IconSize::Small)
                    .tooltip(Tooltip::text(t!("Close")))
                    .on_click(
                        cx.listener(|this, _, window, cx| this.dismiss_side_pane(window, cx)),
                    ),
            );
        let pane = div()
            .id("agent-side-pane")
            .key_context("AgentSidePane")
            .on_action(
                cx.listener(|this, _: &menu::Cancel, window, cx| this.cancel_side_pane(window, cx)),
            )
            .h_full()
            .flex_none()
            .overflow_hidden()
            .border_l_1()
            .border_color(hairline(0.08, cx))
            .child(
                v_flex()
                    .h_full()
                    .w(px(width))
                    .child(header)
                    .child(div().flex_1().min_h_0().child(body)),
            )
            .with_animation(
                ("agent-side-pane-slide", pane.opening),
                Animation::new(SLIDE_IN).with_easing(ease_out_quint()),
                move |this, delta| this.w(px(width * delta)),
            );
        let drop_highlight = cx.theme().colors().drop_target_background;
        // The chat takes its final width at once, so the slide doesn't rewrap it every frame.
        h_flex()
            .size_full()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .overflow_hidden()
                    .bg(background)
                    .drag_over::<ExternalPaths>(move |style, _, _, _| style.bg(drop_highlight))
                    .drag_over::<DraggedSelection>(move |style, _, _, _| style.bg(drop_highlight))
                    .child(
                        v_flex()
                            .h_full()
                            .w(px((available - width).max(0.)))
                            .child(chat),
                    ),
            )
            .child(pane)
            .into_any_element()
    }
}
