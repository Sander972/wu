use super::{ChatView, claude_dialogs::show_toast, icon, links::FileTarget};
use crate::chat_style::{Chip, format_elapsed, ink, popover_card, text_faint, ui};
use crate::session::AgentSession;
use crate::usage_rings::{menu_heading, with_separators};
use agent_harness::{BackgroundTaskKind, ToolCall};
use anyhow::Result;
use collections::HashMap;
use gpui::{
    Anchor, AnyElement, App, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable,
    IntoElement, ParentElement, Render, SharedString, Styled, Task, WeakEntity, Window, div, point,
    px,
};
use i18n::t;
use serde_json::{Value, json};
use std::{path::PathBuf, time::Duration};
use theme::ActiveTheme as _;
use ui::{PopoverMenu, Tooltip, prelude::*};
use util::ResultExt as _;

const TICK: Duration = Duration::from_secs(1);
const OUTPUT_POLL: Duration = Duration::from_secs(2);
const OUTPUT_TAIL_LINES: usize = 4;
const CARD_WIDTH: f32 = 360.;

#[derive(Clone)]
struct TaskRow {
    task_id: String,
    kind: BackgroundTaskKind,
    title: SharedString,
    command: Option<SharedString>,
    elapsed: SharedString,
    subagent: Option<(String, ToolCall)>,
    output_file: Option<PathBuf>,
    total_tokens: Option<u64>,
}

// Claude's backgrounded Bash result reads "... Output is being written to: <path>.output. ...".
fn output_file(tool_output: &str) -> Option<PathBuf> {
    const MARKER: &str = "Output is being written to: ";
    const EXTENSION: &str = ".output";
    let start = tool_output.find(MARKER)? + MARKER.len();
    let rest = &tool_output[start..];
    let end = rest.find(EXTENSION)? + EXTENSION.len();
    Some(PathBuf::from(&rest[..end]))
}

/// The last lines of a task's output, each as a terminal leaves it once carriage returns have redrawn it.
fn output_tail(output: &str) -> SharedString {
    let lines = output
        .lines()
        .filter_map(|line| line.rsplit('\r').find(|segment| !segment.trim().is_empty()))
        .collect::<Vec<_>>();
    lines[lines.len().saturating_sub(OUTPUT_TAIL_LINES)..]
        .join("\n")
        .into()
}

fn running_label(kind: BackgroundTaskKind) -> SharedString {
    match kind {
        BackgroundTaskKind::Shell => t!("Command running"),
        BackgroundTaskKind::Agent => t!("Agent running"),
        BackgroundTaskKind::Other => t!("Task running"),
    }
}

fn untitled_task(kind: BackgroundTaskKind) -> SharedString {
    match kind {
        BackgroundTaskKind::Shell => t!("Background command"),
        BackgroundTaskKind::Agent => t!("Background agent"),
        BackgroundTaskKind::Other => t!("Background task"),
    }
}

impl ChatView {
    pub(super) fn ensure_background_tick(&mut self, cx: &mut Context<Self>) {
        if self.session.read(cx).background_tasks().is_empty() || self.background_ticking {
            return;
        }
        self.background_ticking = true;
        self._background_tick = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(TICK).await;
                let running = this
                    .update(cx, |this, cx| {
                        let running = !this.session.read(cx).background_tasks().is_empty();
                        if running {
                            cx.notify();
                        } else {
                            this.background_ticking = false;
                        }
                        running
                    })
                    .unwrap_or(false);
                if !running {
                    return;
                }
            }
        });
    }

    pub(super) fn show_background_tasks(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.session.read(cx).background_tasks().is_empty() {
            show_toast(&self.workspace, t!("No background tasks are running."), cx);
            return;
        }
        self.background_card.show(window, cx);
    }

    pub(super) fn render_background_indicator(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let tasks = self.session.read(cx).background_tasks();
        // The card stays open after its last task stops, so the stopped row can still show as canceled.
        if tasks.is_empty() && !self.background_card.is_deployed() {
            return None;
        }
        let label: SharedString = match tasks {
            [] => t!("Background tasks"),
            [task] => running_label(task.kind),
            _ if tasks
                .iter()
                .all(|task| task.kind == BackgroundTaskKind::Agent) =>
            {
                t!("{count} agents", count = tasks.len())
            }
            _ => t!("{count} tasks running", count = tasks.len()),
        };
        let elapsed = tasks
            .iter()
            .map(|task| task.started_at)
            .min()
            .map(|oldest| format_elapsed(oldest.elapsed().as_secs()))
            .unwrap_or_default();
        let elapsed_placeholder = elapsed
            .chars()
            .map(|character| {
                if character.is_ascii_digit() {
                    '0'
                } else {
                    character
                }
            })
            .collect::<String>();
        let colors = cx.theme().colors();
        let session = self.session.clone();
        let chat = cx.weak_entity();
        let summary = h_flex()
            .debug_selector(|| "agent-background-tasks".into())
            .h(px(20.))
            .px(px(7.))
            .gap(px(5.))
            .text_size(ui(11.))
            .whitespace_nowrap()
            .child(
                div()
                    .size(px(6.))
                    .rounded_full()
                    .bg(cx.theme().status().info),
            )
            .child(label)
            .child(
                div()
                    .relative()
                    .text_color(text_faint(cx))
                    .child(div().invisible().child(elapsed_placeholder))
                    .child(div().absolute().top_0().left_0().child(elapsed)),
            );
        Some(
            PopoverMenu::new(SharedString::from(format!(
                "agent-background-tasks-{}",
                cx.entity_id()
            )))
            .trigger(
                Chip::new("agent-background-tasks", 6., summary)
                    .hover_background(ink(0.06, cx))
                    .text_colors(colors.text_muted, colors.text),
            )
            .menu(move |_, cx| {
                let session = session.clone();
                let chat = chat.clone();
                Some(cx.new(|cx| BackgroundTasksCard::new(session, chat, cx)))
            })
            .with_handle(self.background_card.clone())
            .anchor(Anchor::BottomLeft)
            .attach(Anchor::TopLeft)
            .offset(point(px(0.), px(-6.)))
            .into_any_element(),
        )
    }

    pub(super) fn render_run_in_background(
        &self,
        tool_use_id: &str,
        cx: &Context<Self>,
    ) -> Option<AnyElement> {
        if !self.session.read(cx).can_move_to_background(tool_use_id) {
            return None;
        }
        let tool_use_id = tool_use_id.to_string();
        Some(
            Button::new(
                SharedString::from(format!("agent-run-in-background-{tool_use_id}")),
                t!("Run in background"),
            )
            .label_size(LabelSize::XSmall)
            .on_click(cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                this.run_in_background(tool_use_id.clone(), cx);
            }))
            .into_any_element(),
        )
    }

    pub(super) fn run_in_background(&mut self, tool_use_id: String, cx: &mut Context<Self>) {
        let request = json!({ "subtype": "background_tasks", "tool_use_id": tool_use_id });
        let Some(answer) = live_control(&self.session, request, cx) else {
            return;
        };
        let workspace = self.workspace.clone();
        cx.spawn(async move |_, cx| {
            let message = match answer.await {
                Ok(response) if response["backgrounded"].as_bool() != Some(false) => return,
                Ok(_) => t!(
                    "Could not move this to the background. It may have just finished or already be running in the background."
                ),
                Err(error) => error.to_string().into(),
            };
            show_toast(&workspace, message, cx);
        })
        .detach();
    }
}

/// Without a live run `control` would start a one-off CLI, which has none of this chat's tasks.
fn live_control(
    session: &Entity<AgentSession>,
    request: Value,
    cx: &mut App,
) -> Option<Task<Result<Value>>> {
    session.update(cx, |session, cx| {
        session.has_live_run().then(|| session.control(request, cx))
    })
}

fn background_rows(session: &AgentSession) -> Vec<TaskRow> {
    session
        .background_tasks()
        .iter()
        .map(|task| {
            let tool = task
                .tool_use_id
                .as_deref()
                .and_then(|id| session.tool_entry(id));
            let command = tool.and_then(|tool| match &tool.call {
                ToolCall::Exec { command } => Some(SharedString::from(command.clone())),
                _ => None,
            });
            let subagent = tool
                .filter(|tool| tool.call.is_subagent_spawn())
                .map(|tool| (tool.id.clone(), tool.call.clone()));
            let output_file = tool
                .and_then(|tool| tool.output.as_deref())
                .and_then(output_file);
            let title: SharedString = if !task.description.trim().is_empty() {
                task.description.clone().into()
            } else if let Some(command) = &command {
                command.clone()
            } else {
                untitled_task(task.kind)
            };
            TaskRow {
                task_id: task.task_id.clone(),
                kind: task.kind,
                command: command.filter(|command| *command != title),
                title,
                elapsed: format_elapsed(task.started_at.elapsed().as_secs()).into(),
                subagent,
                output_file,
                total_tokens: task.total_tokens,
            }
        })
        .collect()
}

pub(super) struct BackgroundTasksCard {
    session: Entity<AgentSession>,
    chat: WeakEntity<ChatView>,
    focus_handle: FocusHandle,
    canceled: Vec<TaskRow>,
    outputs: HashMap<String, SharedString>,
    _tick: Task<()>,
    _poll_output: Task<()>,
}

impl BackgroundTasksCard {
    fn new(
        session: Entity<AgentSession>,
        chat: WeakEntity<ChatView>,
        cx: &mut Context<Self>,
    ) -> Self {
        let tick = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(TICK).await;
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    return;
                }
            }
        });
        let poll_output = cx.spawn(async move |this, cx| {
            loop {
                let Ok(requests) = this.update(cx, |card, cx| card.request_output(cx)) else {
                    return;
                };
                for (task_id, request) in requests {
                    let output = match request.await {
                        Ok(response) => output_tail(response["output"].as_str().unwrap_or("")),
                        Err(error) => {
                            log::debug!("couldn't read the output of task {task_id}: {error:#}");
                            continue;
                        }
                    };
                    let stored = this.update(cx, |card, cx| {
                        if !output.is_empty() {
                            card.outputs.insert(task_id, output);
                            cx.notify();
                        }
                    });
                    if stored.is_err() {
                        return;
                    }
                }
                cx.background_executor().timer(OUTPUT_POLL).await;
            }
        });
        Self {
            session,
            chat,
            focus_handle: cx.focus_handle(),
            canceled: Vec::new(),
            outputs: HashMap::default(),
            _tick: tick,
            _poll_output: poll_output,
        }
    }

    fn request_output(&mut self, cx: &mut App) -> Vec<(String, Task<Result<Value>>)> {
        let shell_tasks = self
            .session
            .read(cx)
            .background_tasks()
            .iter()
            .filter(|task| task.kind == BackgroundTaskKind::Shell)
            .map(|task| task.task_id.clone())
            .collect::<Vec<_>>();
        shell_tasks
            .into_iter()
            .filter_map(|task_id| {
                let request = json!({ "subtype": "get_task_output", "task_id": task_id });
                let output = live_control(&self.session, request, cx)?;
                Some((task_id, output))
            })
            .collect()
    }

    fn stop(&mut self, task_id: String, cx: &mut Context<Self>) {
        let Some(row) = background_rows(self.session.read(cx))
            .into_iter()
            .find(|row| row.task_id == task_id)
        else {
            return;
        };
        let request = json!({ "subtype": "stop_task", "task_id": task_id });
        let Some(stopped) = live_control(&self.session, request, cx) else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let stopped = stopped.await;
            this.update(cx, |card, cx| match stopped {
                Ok(_) => {
                    if !card
                        .canceled
                        .iter()
                        .any(|canceled| canceled.task_id == row.task_id)
                    {
                        card.canceled.push(row);
                    }
                    cx.notify();
                }
                Err(error) => {
                    log::warn!("couldn't stop task {}: {error:#}", row.task_id);
                    card.chat
                        .update(cx, |chat, cx| {
                            show_toast(
                                &chat.workspace,
                                t!("The task could not be stopped. It may have finished already."),
                                cx,
                            )
                        })
                        .log_err();
                }
            })
            .log_err();
        })
        .detach();
    }
}

impl EventEmitter<DismissEvent> for BackgroundTasksCard {}

impl Focusable for BackgroundTasksCard {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for BackgroundTasksCard {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let session = self.session.read(cx);
        let live_run = session.has_live_run();
        let rows = background_rows(session)
            .into_iter()
            .filter(|row| {
                !self
                    .canceled
                    .iter()
                    .any(|canceled| canceled.task_id == row.task_id)
            })
            .map(|row| (row, false))
            .chain(self.canceled.iter().cloned().map(|row| (row, true)))
            .collect::<Vec<_>>();
        let outputs = self.outputs.clone();
        let muted = cx.theme().colors().text_muted;
        let faint = text_faint(cx);
        let code_font = super::code_font(cx);
        let hover = ink(0.05, cx);
        let chat = self.chat.clone();
        popover_card(cx)
            .key_context("menu")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|_, _: &menu::Cancel, _, cx| cx.emit(DismissEvent)))
            .on_mouse_down_out(cx.listener(|_, _, _, cx| cx.emit(DismissEvent)))
            .w(px(CARD_WIDTH))
            .child(menu_heading(&t!("Background tasks"), cx))
            .when(rows.is_empty(), |this| {
                this.child(
                    div()
                        .px(px(8.))
                        .pb(px(6.))
                        .text_size(ui(12.))
                        .text_color(muted)
                        .child(t!("Nothing is running in the background.")),
                )
            })
            .child(
                v_flex().px(px(4.)).pb(px(4.)).gap(px(2.)).children(
                    rows.into_iter()
                        .enumerate()
                        .map(move |(index, (row, canceled))| {
                            let task_id = row.task_id.clone();
                            let output = outputs.get(&row.task_id).cloned();
                            v_flex()
                                .id(index)
                                .px(px(4.))
                                .py(px(2.))
                                .rounded(px(6.))
                                .gap(px(2.))
                                .text_size(ui(12.))
                                .when_some(row.subagent, |this, (id, call)| {
                                    let chat = chat.clone();
                                    this.cursor_pointer()
                                        .hover(|style| style.bg(hover))
                                        .tooltip(Tooltip::text(t!("Open subagent")))
                                        .on_click(cx.listener(move |_, _, window, cx| {
                                            chat.update(cx, |chat, cx| {
                                                chat.open_subagent(id.clone(), &call, window, cx)
                                            })
                                            .log_err();
                                            cx.emit(DismissEvent);
                                        }))
                                })
                                .when_some(row.output_file, |this, path| {
                                    let chat = chat.clone();
                                    this.cursor_pointer()
                                        .hover(|style| style.bg(hover))
                                        .tooltip(Tooltip::text(t!("Show output")))
                                        .on_click(cx.listener(move |_, _, window, cx| {
                                            let target = FileTarget {
                                                path: path.clone(),
                                                line: None,
                                            };
                                            chat.update(cx, |chat, cx| {
                                                chat.open_file(target, window, cx)
                                            })
                                            .log_err();
                                            cx.emit(DismissEvent);
                                        }))
                                })
                                .child(
                                    h_flex()
                                        .gap(px(8.))
                                        .child(
                                            icon(
                                                match row.kind {
                                                    BackgroundTaskKind::Shell => {
                                                        IconName::AgentTerminal
                                                    }
                                                    BackgroundTaskKind::Agent => IconName::AgentBot,
                                                    BackgroundTaskKind::Other => {
                                                        IconName::AgentWidget
                                                    }
                                                },
                                                14.,
                                            )
                                            .text_color(muted),
                                        )
                                        .child(div().flex_1().min_w_0().truncate().child(row.title))
                                        .when_some(row.total_tokens, |this, tokens| {
                                            this.child(div().flex_none().text_color(faint).child(
                                                t!(
                                                    "{count} tokens",
                                                    count = with_separators(tokens)
                                                ),
                                            ))
                                        })
                                        .child(div().flex_none().text_color(faint).child(
                                            if canceled {
                                                t!("Canceled")
                                            } else {
                                                row.elapsed
                                            },
                                        ))
                                        .when(live_run && !canceled, |this| {
                                            this.child(
                                                Button::new(
                                                    SharedString::from(format!(
                                                        "agent-stop-task-{index}"
                                                    )),
                                                    t!("Stop"),
                                                )
                                                .label_size(LabelSize::XSmall)
                                                .on_click(cx.listener(move |card, _, _, cx| {
                                                    cx.stop_propagation();
                                                    card.stop(task_id.clone(), cx);
                                                })),
                                            )
                                        }),
                                )
                                .when_some(row.command, |this, command| {
                                    this.child(
                                        div()
                                            .pl(px(22.))
                                            .text_size(ui(11.))
                                            .text_color(muted)
                                            .font(code_font.clone())
                                            .child(command),
                                    )
                                })
                                .when_some(output, |this, output| {
                                    this.child(
                                        div()
                                            .pl(px(22.))
                                            .text_size(ui(11.))
                                            .text_color(faint)
                                            .font(code_font.clone())
                                            .child(output),
                                    )
                                })
                        }),
                ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        AgentKind,
        session::{
            AgentStore,
            tests::{attach_live_run, feed_events, new_store},
        },
    };
    use agent_harness::AgentEvent;
    use gpui::{TestAppContext, WeakEntity};
    use project::FakeFs;

    #[gpui::test]
    async fn the_popup_lists_each_task_once(cx: &mut TestAppContext) {
        cx.update(|cx| {
            workspace::AppState::test(cx);
            editor::init(cx);
        });
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = new_store(directory.path(), cx);
        store.update(cx, |store, _| store.skip_plan_usage());
        let project = project::Project::test(FakeFs::new(cx.executor()), [], cx).await;
        let session = store.update(cx, |store, cx| {
            store.create_session(AgentKind::Claude, directory.path().to_path_buf(), cx)
        });
        feed_events(
            &session,
            vec![
                AgentEvent::ToolCall {
                    id: "toolu_bash".into(),
                    call: ToolCall::Exec {
                        command: "cargo test --workspace".into(),
                    },
                },
                AgentEvent::ToolResult {
                    id: "toolu_bash".into(),
                    output: Some("Command running in background with ID: bg1. Output is being written to: C:\\Temp\\claude\\tasks\\bg1.output. You will be notified when it completes.".into()),
                    is_error: false,
                    diff: None,
                },
                AgentEvent::ToolCall {
                    id: "toolu_agent".into(),
                    call: ToolCall::Unknown {
                        name: "Agent: scan the repo".into(),
                        input: None,
                    },
                },
                AgentEvent::TaskStarted {
                    task_id: "bg1".into(),
                    tool_use_id: Some("toolu_bash".into()),
                    kind: BackgroundTaskKind::Shell,
                    description: String::new(),
                },
                AgentEvent::TaskStarted {
                    task_id: "a1".into(),
                    tool_use_id: Some("toolu_agent".into()),
                    kind: BackgroundTaskKind::Agent,
                    description: "Scan the repo".into(),
                },
                AgentEvent::BackgroundTasksChanged {
                    tasks: vec![
                        agent_harness::BackgroundTaskInfo {
                            task_id: "bg1".into(),
                            kind: BackgroundTaskKind::Shell,
                            description: String::new(),
                        },
                        agent_harness::BackgroundTaskInfo {
                            task_id: "a1".into(),
                            kind: BackgroundTaskKind::Agent,
                            description: String::new(),
                        },
                    ],
                },
            ],
            cx,
        );
        let (chat, cx) = cx.add_window_view(|window, cx| {
            ChatView::new(
                session.clone(),
                store.clone(),
                project,
                WeakEntity::new_invalid(),
                window,
                cx,
            )
        });
        chat.update(cx, |chat, cx| {
            assert!(chat.render_background_indicator(cx).is_some());
        });
        session.read_with(cx, |session, _| {
            let rows = background_rows(session);
            assert_eq!(rows.len(), 2);
            assert_eq!(rows[0].title.as_ref(), "cargo test --workspace");
            assert!(
                rows[0].command.is_none(),
                "the command is the title already"
            );
            assert!(rows[0].subagent.is_none(), "commands don't open a subagent");
            assert_eq!(
                rows[0].output_file.as_deref(),
                Some(std::path::Path::new("C:\\Temp\\claude\\tasks\\bg1.output"))
            );
            assert!(rows[1].output_file.is_none());
            assert_eq!(rows[1].title.as_ref(), "Scan the repo");
            assert!(rows[1].command.is_none());
            assert_eq!(
                rows[1].subagent.as_ref().map(|(id, _)| id.as_str()),
                Some("toolu_agent")
            );
        });
        feed_events(
            &session,
            vec![AgentEvent::BackgroundTasksChanged { tasks: Vec::new() }],
            cx,
        );
        chat.update(cx, |chat, cx| {
            assert!(chat.render_background_indicator(cx).is_none())
        });
    }

    fn shell_task(task_id: &str) -> Vec<AgentEvent> {
        vec![
            AgentEvent::TaskStarted {
                task_id: task_id.into(),
                tool_use_id: None,
                kind: BackgroundTaskKind::Shell,
                description: format!("Task {task_id}"),
            },
            AgentEvent::BackgroundTasksChanged {
                tasks: vec![agent_harness::BackgroundTaskInfo {
                    task_id: task_id.into(),
                    kind: BackgroundTaskKind::Shell,
                    description: String::new(),
                }],
            },
        ]
    }

    fn next_control(
        receiver: &mut tokio::sync::mpsc::Receiver<agent_harness::Steer>,
    ) -> agent_harness::ControlCall {
        match receiver.try_recv() {
            Ok(agent_harness::Steer::Control(call)) => call,
            Ok(_) => panic!("expected a control request, got another steer"),
            Err(error) => panic!("expected a control request: {error}"),
        }
    }

    fn card_session(
        cx: &mut TestAppContext,
    ) -> (Entity<AgentStore>, Entity<AgentSession>, tempfile::TempDir) {
        cx.update(|cx| {
            workspace::AppState::test(cx);
            editor::init(cx);
        });
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = new_store(directory.path(), cx);
        store.update(cx, |store, _| store.skip_plan_usage());
        let session = store.update(cx, |store, cx| {
            store.create_session(AgentKind::Claude, directory.path().to_path_buf(), cx)
        });
        (store, session, directory)
    }

    #[gpui::test]
    async fn stop_sends_stop_task_for_that_id_only(cx: &mut TestAppContext) {
        let (_store, session, _directory) = card_session(cx);
        let mut events = shell_task("bg1");
        events.extend(shell_task("bg2"));
        events.push(AgentEvent::BackgroundTasksChanged {
            tasks: ["bg1", "bg2"]
                .map(|task_id| agent_harness::BackgroundTaskInfo {
                    task_id: task_id.into(),
                    kind: BackgroundTaskKind::Shell,
                    description: String::new(),
                })
                .into(),
        });
        feed_events(&session, events, cx);
        let card =
            cx.new(|cx| BackgroundTasksCard::new(session.clone(), WeakEntity::new_invalid(), cx));
        cx.run_until_parked();
        let mut receiver = attach_live_run(&session, cx);

        card.update(cx, |card, cx| card.stop("bg2".into(), cx));
        cx.run_until_parked();
        let call = next_control(&mut receiver);
        assert_eq!(
            call.request,
            serde_json::json!({ "subtype": "stop_task", "task_id": "bg2" })
        );
        assert!(receiver.try_recv().is_err(), "only one task is stopped");
        call.reply
            .send(Ok(serde_json::json!({})))
            .expect("the card waits for the reply");
        feed_events(
            &session,
            vec![AgentEvent::TaskFinished {
                task_id: "bg2".into(),
                status: agent_harness::DoneStatus::Interrupted,
            }],
            cx,
        );
        cx.run_until_parked();
        card.read_with(cx, |card, cx| {
            let rows = background_rows(card.session.read(cx));
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].task_id, "bg1", "the other task keeps running");
            assert_eq!(
                card.canceled
                    .iter()
                    .map(|row| row.task_id.as_str())
                    .collect::<Vec<_>>(),
                ["bg2"],
                "the stopped task stays listed as canceled"
            );
        });
    }

    #[gpui::test]
    async fn background_button_sends_the_tool_use_id(cx: &mut TestAppContext) {
        let (store, session, _directory) = card_session(cx);
        let project = project::Project::test(FakeFs::new(cx.executor()), [], cx).await;
        feed_events(
            &session,
            vec![
                AgentEvent::ToolCall {
                    id: "toolu_bash".into(),
                    call: ToolCall::Exec {
                        command: "sleep 120".into(),
                    },
                },
                AgentEvent::ToolCall {
                    id: "toolu_other".into(),
                    call: ToolCall::Exec {
                        command: "sleep 5".into(),
                    },
                },
                AgentEvent::TaskStarted {
                    task_id: "fg1".into(),
                    tool_use_id: Some("toolu_bash".into()),
                    kind: BackgroundTaskKind::Shell,
                    description: String::new(),
                },
            ],
            cx,
        );
        let (chat, cx) = cx.add_window_view(|window, cx| {
            ChatView::new(
                session.clone(),
                store,
                project,
                WeakEntity::new_invalid(),
                window,
                cx,
            )
        });
        session.read_with(cx, |session, _| {
            assert!(
                !session.can_move_to_background("toolu_bash"),
                "without a live run there is nothing to move"
            );
        });
        let mut receiver = attach_live_run(&session, cx);
        session.read_with(cx, |session, _| {
            assert!(session.can_move_to_background("toolu_bash"));
            assert!(
                !session.can_move_to_background("toolu_other"),
                "a tool Claude reported no task for can't move"
            );
        });

        chat.update(cx, |chat, cx| {
            chat.run_in_background("toolu_bash".into(), cx)
        });
        cx.run_until_parked();
        let call = next_control(&mut receiver);
        assert_eq!(
            call.request,
            serde_json::json!({ "subtype": "background_tasks", "tool_use_id": "toolu_bash" })
        );
        assert!(receiver.try_recv().is_err());
    }

    #[gpui::test]
    async fn output_is_requested_when_the_card_opens_then_every_poll(cx: &mut TestAppContext) {
        let (_store, session, _directory) = card_session(cx);
        feed_events(&session, shell_task("bg1"), cx);
        let mut receiver = attach_live_run(&session, cx);
        let _card =
            cx.new(|cx| BackgroundTasksCard::new(session.clone(), WeakEntity::new_invalid(), cx));
        cx.run_until_parked();
        next_control(&mut receiver);
        assert!(receiver.try_recv().is_err());
        cx.executor().advance_clock(OUTPUT_POLL);
        cx.run_until_parked();
        next_control(&mut receiver);
    }

    #[gpui::test]
    async fn output_polling_stops_when_the_card_closes(cx: &mut TestAppContext) {
        let (_store, session, _directory) = card_session(cx);
        feed_events(&session, shell_task("bg1"), cx);
        let card =
            cx.new(|cx| BackgroundTasksCard::new(session.clone(), WeakEntity::new_invalid(), cx));
        assert!(
            card.update(cx, |card, cx| card.request_output(cx).is_empty()),
            "without a live run polling would start a one-off CLI, so it asks nothing"
        );

        let mut receiver = attach_live_run(&session, cx);
        cx.executor().advance_clock(OUTPUT_POLL);
        cx.run_until_parked();
        let call = next_control(&mut receiver);
        assert_eq!(
            call.request,
            serde_json::json!({ "subtype": "get_task_output", "task_id": "bg1" })
        );
        call.reply
            .send(Ok(serde_json::json!({
                "output": "one\ntwo\nthree\nfour\n50%\r100%\r",
                "total_bytes": 30,
                "truncated": false,
            })))
            .expect("the card waits for the reply");
        cx.run_until_parked();
        card.read_with(cx, |card, _| {
            assert_eq!(
                card.outputs.get("bg1").map(|output| output.as_ref()),
                Some("two\nthree\nfour\n100%")
            );
        });

        drop(card);
        cx.run_until_parked();
        cx.executor().advance_clock(OUTPUT_POLL * 5);
        cx.run_until_parked();
        assert!(receiver.try_recv().is_err(), "a closed card stops polling");
    }
}
