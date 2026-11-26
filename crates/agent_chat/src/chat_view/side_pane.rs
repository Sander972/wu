use super::{ChatView, SubagentRef, attachments, links::FileTarget};
use crate::chat_style::hairline;
use editor::Editor;
use gpui::{
    Animation, AnimationExt as _, AnyElement, App, Context, Entity, ExternalPaths, FocusHandle,
    Focusable, IntoElement, ParentElement, SharedString, Styled, Task, Window, canvas,
    ease_out_quint, px,
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
}

/// A `/btw` question that has no answer in the session yet.
pub(super) enum PendingSideQuestion {
    Asking(String),
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
        if let Some(focus_handle) = content.focus_handle(cx) {
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
        self.pending_side_question = Some(PendingSideQuestion::Asking(question.clone()));
        let reply = self.session.update(cx, |session, cx| {
            session.control(
                serde_json::json!({ "subtype": "side_question", "question": question }),
                cx,
            )
        });
        self._side_question = cx.spawn(async move |this, cx| {
            let outcome = reply.await;
            this.update(cx, |this, cx| {
                let answer = outcome
                    .as_ref()
                    .ok()
                    .and_then(|reply| reply["response"].as_str());
                if let Some(answer) = answer {
                    this.pending_side_question = None;
                    this.session.update(cx, |session, cx| {
                        session.push_side_answer(question, answer.to_string(), cx)
                    });
                } else {
                    let message = match outcome {
                        Ok(_) => t!("The agent had no answer to that question."),
                        Err(error) => error.to_string().into(),
                    };
                    this.pending_side_question =
                        Some(PendingSideQuestion::Failed { question, message });
                }
                cx.notify();
            })
            .log_err();
        });
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
        let pending = self.pending_side_question.as_ref().map(|pending| {
            let (question, note) = match pending {
                PendingSideQuestion::Asking(question) => (
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
        let empty = pending.is_none() && answers.is_empty();
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
            SidePaneContent::SideQuestions => {}
        }
        cx.notify();
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
            .child(
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
            .child(
                IconButton::new("agent-side-pane-open-in-tab", IconName::Maximize)
                    .icon_size(IconSize::Small)
                    .tooltip(Tooltip::text(t!("Open in Tab")))
                    .on_click(
                        cx.listener(|this, _, window, cx| this.move_side_pane_to_tab(window, cx)),
                    ),
            )
            .child(
                IconButton::new("agent-side-pane-close", IconName::Close)
                    .icon_size(IconSize::Small)
                    .tooltip(Tooltip::text(t!("Close")))
                    .on_click(cx.listener(|this, _, window, cx| this.close_side_pane(window, cx))),
            );
        let pane = div()
            .id("agent-side-pane")
            .key_context("AgentSidePane")
            .on_action(
                cx.listener(|this, _: &menu::Cancel, window, cx| this.close_side_pane(window, cx)),
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
