use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::StreamExt;
use tokio::sync::{mpsc, oneshot};

use agent_harness::{
    AgentEvent, DoneStatus, HarnessId, PermissionMode, RunRequest, ToolCall, UserInputAnswer,
    UserInputQuestion,
};
use agent_harness::{
    CancellationToken, ClaudeHarness, ControlCall, Harness, HarnessError, RunControls, Steer,
    SteerMessage,
};

#[cfg(windows)]
const FAKE_CLAUDE: &str = "fake-claude.cmd";
#[cfg(not(windows))]
const FAKE_CLAUDE: &str = "fake-claude.sh";

fn fixture_path() -> PathBuf {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(FAKE_CLAUDE);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Err(error) = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
        {
            eprintln!("could not mark fake-claude.sh executable: {error}");
        }
    }
    path
}

fn harness() -> ClaudeHarness {
    ClaudeHarness::new()
        .with_executable(fixture_path())
        .with_config_dir(fixture_path().with_file_name("claude-config"))
}

fn request(prompt: &str) -> RunRequest {
    RunRequest {
        prompt: prompt.into(),
        model: None,
        reasoning: None,
        model_options: serde_json::Map::new(),
        cwd: String::new(),
        permission: PermissionMode::FullAccess,
        resume: None,
        fork: None,
        attachments: Vec::new(),
        skills: Vec::new(),
        auto_compact_tokens: None,
        chrome: false,
        drop_prompt: None,
        ephemeral: false,
        resume_interrupted: false,
        prompt_suggestions: false,
        launch: Default::default(),
        context: Vec::new(),
    }
}

fn controls(answer_label: &'static str) -> (RunControls, mpsc::Sender<Steer>, CancellationToken) {
    let (steer_tx, steer_rx) = mpsc::channel(8);
    let token = CancellationToken::new();
    let controls = RunControls {
        request_input: Box::new(move |questions| {
            let (tx, rx) = oneshot::channel();
            let answers: Vec<UserInputAnswer> = questions
                .iter()
                .map(|q| UserInputAnswer {
                    question_id: q.id.clone(),
                    labels: vec![answer_label.into()],
                    decision: None,
                })
                .collect();
            tx.send(answers).expect("receiver is still held");
            rx
        }),
        steering: steer_rx,
        interrupt: token.clone(),
        permission: tokio::sync::watch::channel(agent_harness::PermissionMode::Ask).1,
        before_file_tool: agent_harness::file_tools_go_ahead(),
        mcp: agent_harness::no_mcp_servers(),
    };
    (controls, steer_tx, token)
}

async fn run_to_end(
    harness: &ClaudeHarness,
    req: RunRequest,
    controls: RunControls,
) -> Vec<AgentEvent> {
    let stream = harness.run(req, controls).await.expect("run starts");
    tokio::time::timeout(
        Duration::from_secs(10),
        stream.map(|r| r.expect("stream event")).collect::<Vec<_>>(),
    )
    .await
    .expect("run finished in time")
}

fn harness_with_saved_chat() -> (tempfile::TempDir, ClaudeHarness) {
    let config = tempfile::tempdir().expect("temporary directory");
    let project = config.path().join("projects").join("-tmp");
    std::fs::create_dir_all(&project).expect("project dir");
    let records = [
        r#"{"type":"user","uuid":"ask","parentUuid":null,"message":{"content":"hi"}}"#,
        r#"{"type":"assistant","uuid":"answer","parentUuid":"ask","message":{"content":[]}}"#,
        r#"{"type":"system","uuid":"timing","parentUuid":"answer"}"#,
        r#"{"type":"user","uuid":"typo","parentUuid":"timing","message":{"content":"fix teh bug"}}"#,
    ];
    std::fs::write(project.join("sess-drop.jsonl"), records.join("\n")).expect("chat file");
    let harness = ClaudeHarness::new()
        .with_executable(fixture_path())
        .with_config_dir(config.path());
    (config, harness)
}

#[tokio::test]
async fn a_dropped_prompt_resumes_the_chat_just_before_it() {
    let (_config, harness) = harness_with_saved_chat();
    let reply = |events: &[AgentEvent]| -> String {
        events
            .iter()
            .filter_map(|event| match event {
                AgentEvent::TextDelta { text } => Some(text.as_str()),
                _ => None,
            })
            .collect()
    };

    let events = run_to_end(&harness, request("scenario:drop"), controls("Allow").0).await;
    assert!(
        events
            .iter()
            .any(|event| matches!(event, AgentEvent::PromptRecorded { id } if !id.is_empty())),
        "the replayed prompt reports its id: {events:?}"
    );
    assert_eq!(reply(&events), "fresh");

    let mut dropping = request("scenario:drop");
    dropping.resume = Some("sess-drop".into());
    dropping.drop_prompt = Some("typo".into());
    let events = run_to_end(&harness, dropping, controls("Allow").0).await;
    assert_eq!(reply(&events), "at answer");

    let mut first = request("scenario:drop");
    first.resume = Some("sess-drop".into());
    first.drop_prompt = Some("ask".into());
    let events = run_to_end(&harness, first, controls("Allow").0).await;
    assert_eq!(
        reply(&events),
        "fresh",
        "dropping the first prompt starts over"
    );

    let mut missing = request("scenario:drop");
    missing.resume = Some("sess-drop".into());
    missing.drop_prompt = Some("gone".into());
    let events = run_to_end(&harness, missing, controls("Allow").0).await;
    assert_eq!(reply(&events), "whole");
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::Error { message } if message.contains("couldn't find your earlier message")
    )));
}

#[tokio::test]
async fn fork_with_drop_prompt_resumes_at_the_earlier_message() {
    let (_config, harness) = harness_with_saved_chat();

    let mut fork = request("scenario:drop");
    fork.fork = Some("sess-drop".into());
    fork.drop_prompt = Some("typo".into());
    let events = run_to_end(&harness, fork, controls("Allow").0).await;
    assert_eq!(reply_text(&events), "at answer, forked from sess-drop");

    let mut fork_at_first = request("scenario:drop");
    fork_at_first.fork = Some("sess-drop".into());
    fork_at_first.drop_prompt = Some("ask".into());
    let events = run_to_end(&harness, fork_at_first, controls("Allow").0).await;
    assert_eq!(
        reply_text(&events),
        "fresh",
        "forking at the first prompt starts over"
    );
}

#[tokio::test]
async fn permission_changes_reach_a_running_turn() {
    let (mut controls, _steer, _token) = controls("Deny");
    let (permission_tx, permission_rx) = tokio::sync::watch::channel(PermissionMode::Ask);
    controls.permission = permission_rx;
    let mut req = request("scenario:mode-change");
    req.permission = PermissionMode::Ask;
    let stream = harness().run(req, controls).await.expect("run starts");
    permission_tx.send_replace(PermissionMode::FullAccess);
    let events = tokio::time::timeout(
        Duration::from_secs(10),
        stream.map(|r| r.expect("stream event")).collect::<Vec<_>>(),
    )
    .await
    .expect("run finished in time");
    assert!(
        events.iter().any(|event| matches!(
            event,
            AgentEvent::Done {
                status: DoneStatus::Completed,
                ..
            }
        )),
        "{events:?}"
    );
}

#[tokio::test]
async fn a_live_switch_to_plan_writes_the_plan_mode() {
    let (mut controls, _steer, _token) = controls("Deny");
    let (permission_tx, permission_rx) = tokio::sync::watch::channel(PermissionMode::Ask);
    controls.permission = permission_rx;
    let mut req = request("scenario:mode-change");
    req.permission = PermissionMode::Ask;
    let stream = harness().run(req, controls).await.expect("run starts");
    permission_tx.send_replace(PermissionMode::Plan);
    let events = tokio::time::timeout(
        Duration::from_secs(10),
        stream.map(|r| r.expect("stream event")).collect::<Vec<_>>(),
    )
    .await
    .expect("run finished in time");
    assert_eq!(
        done_statuses(&events),
        [DoneStatus::Completed],
        "{events:?}"
    );
}

#[tokio::test]
async fn a_silently_adopted_mode_changes_prompts_without_a_mode_request() {
    let (mut controls, steer, _token) = controls("Deny");
    let (permission_tx, permission_rx) = tokio::sync::watch::channel(PermissionMode::Ask);
    controls.permission = permission_rx;
    let mut req = request("scenario:silent-mode");
    req.permission = PermissionMode::Ask;
    let mut stream = harness().run(req, controls).await.expect("run starts");
    let mut events = vec![
        stream
            .next()
            .await
            .expect("the run reports its start")
            .expect("stream event"),
    ];
    permission_tx.send_if_modified(|mode| {
        *mode = PermissionMode::FullAccess;
        false
    });
    steer
        .send(Steer::Message(SteerMessage {
            prompt: "go on".into(),
            message_id: None,
            attachments: Vec::new(),
            skills: Vec::new(),
            context: Vec::new(),
            settings: None,
        }))
        .await
        .expect("steer queued");
    events.extend(
        tokio::time::timeout(
            Duration::from_secs(10),
            stream.map(|r| r.expect("stream event")).collect::<Vec<_>>(),
        )
        .await
        .expect("run finished in time"),
    );
    assert_eq!(
        done_statuses(&events),
        [DoneStatus::Completed],
        "{events:?}"
    );
}

#[tokio::test]
async fn happy_path_normalizes_events_and_tags_subagents() {
    let (controls, _steer, _token) = controls("A");
    let events = run_to_end(&harness(), request("scenario:happy"), controls).await;

    let starts: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::SessionStarted {
                harness,
                model,
                tools,
                session_id,
                ..
            } => Some((harness, model, tools, session_id)),
            _ => None,
        })
        .collect();
    assert_eq!(starts.len(), 1, "init must be deduped: {events:?}");
    let (h, model, tools, session_id) = starts[0];
    assert_eq!(*h, HarnessId::ClaudeCode);
    assert_eq!(model, "claude-fable-5");
    assert_eq!(tools, &vec!["Bash".to_string(), "Read".to_string()]);
    assert_eq!(session_id, "sess-1");

    assert!(events.contains(&AgentEvent::ReasoningDelta {
        text: "pondering".into()
    }));
    assert!(events.contains(&AgentEvent::TextDelta {
        text: "Hello".into()
    }));

    assert!(
        !events.iter().any(|e| matches!(
            e,
            AgentEvent::TextDelta { text } if text.contains("SUBAGENT")
        )),
        "subagent delta leaked into the parent feed: {events:?}"
    );
    assert!(
        !events.iter().any(|e| matches!(
            e,
            AgentEvent::ToolCall { id, .. } | AgentEvent::ToolResult { id, .. } if id == "sub-tool"
        )),
        "subagent tool frames leaked into the parent feed: {events:?}"
    );
    assert!(events.contains(&AgentEvent::Subagent {
        parent_tool_use_id: "sub-1".into(),
        event: Box::new(AgentEvent::TextDelta {
            text: "SUBAGENT".into()
        }),
    }));
    assert!(events.contains(&AgentEvent::Subagent {
        parent_tool_use_id: "sub-1".into(),
        event: Box::new(AgentEvent::ToolCall {
            id: "sub-tool".into(),
            call: ToolCall::Exec {
                command: "echo sub".into()
            },
        }),
    }));
    assert!(events.contains(&AgentEvent::Subagent {
        parent_tool_use_id: "sub-1".into(),
        event: Box::new(AgentEvent::ToolResult {
            id: "sub-tool".into(),
            is_error: false,
            output: None,
            diff: None,
        }),
    }));

    assert!(events.contains(&AgentEvent::ToolCall {
        id: "tool-1".into(),
        call: ToolCall::Exec {
            command: "ls -la".into()
        },
    }));
    assert!(events.contains(&AgentEvent::ToolCall {
        id: "tool-2".into(),
        call: ToolCall::Mcp {
            server: "linear".into(),
            tool: "search".into(),
            input: Some(serde_json::json!({"q": "bug"})),
        },
    }));
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::AssistantMessageCompleted { .. }))
    );
    assert!(events.contains(&AgentEvent::ToolResult {
        id: "tool-1".into(),
        is_error: false,
        output: None,
        diff: None,
    }));
    assert!(events.contains(&AgentEvent::ToolResult {
        id: "tool-2".into(),
        is_error: true,
        output: None,
        diff: None,
    }));

    assert!(!events.iter().any(|e| matches!(e, AgentEvent::Error { .. })));

    assert!(events.contains(&AgentEvent::Usage {
        input_tokens: 10,
        output_tokens: 20
    }));
    assert_eq!(
        events.last(),
        Some(&AgentEvent::Done {
            status: DoneStatus::Completed,
            result: Some("done!".into()),
            error: None,
            session_id: Some("sess-1".into()),
        })
    );
}

#[tokio::test]
async fn eager_done_forwards_wake_turn_as_second_done() {
    let (controls, _steer, _token) = controls("A");
    let events = run_to_end(&harness(), request("scenario:wake"), controls).await;

    let done_positions: Vec<usize> = events
        .iter()
        .enumerate()
        .filter_map(|(i, e)| matches!(e, AgentEvent::Done { .. }).then_some(i))
        .collect();
    assert_eq!(
        done_positions.len(),
        2,
        "eager done + wake done: {events:?}"
    );

    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, AgentEvent::SessionStarted { .. }))
            .count(),
        1
    );

    let tagged_position = events
        .iter()
        .position(|e| {
            matches!(e, AgentEvent::Subagent { parent_tool_use_id, .. } if parent_tool_use_id == "toolu_agent")
        })
        .expect("tagged subagent traffic present");
    assert!(
        done_positions[0] < tagged_position && tagged_position < done_positions[1],
        "subagent interior must stream between the eager done and the wake done: {events:?}"
    );

    let wake_text = events
        .iter()
        .position(|e| matches!(e, AgentEvent::TextDelta { text } if text == "subagent finished"))
        .expect("wake-turn delta present");
    assert!(done_positions[0] < wake_text && wake_text < done_positions[1]);

    for i in done_positions {
        assert!(matches!(
            &events[i],
            AgentEvent::Done {
                status: DoneStatus::Completed,
                session_id: Some(id),
                ..
            } if id == "sess-wake"
        ));
    }
}

#[tokio::test]
async fn ask_user_question_round_trips_through_the_control_channel() {
    let asked: Arc<Mutex<Vec<UserInputQuestion>>> = Arc::new(Mutex::new(Vec::new()));
    let (steer_tx, steer_rx) = mpsc::channel(8);
    let _steer = steer_tx;
    let token = CancellationToken::new();
    let seen = asked.clone();
    let controls = RunControls {
        request_input: Box::new(move |questions| {
            seen.lock().unwrap().extend(questions.iter().cloned());
            let (tx, rx) = oneshot::channel();
            let answers: Vec<UserInputAnswer> = questions
                .iter()
                .map(|q| UserInputAnswer {
                    question_id: q.id.clone(),
                    labels: vec!["B".into()],
                    decision: None,
                })
                .collect();
            tx.send(answers).expect("receiver is still held");
            rx
        }),
        steering: steer_rx,
        interrupt: token.clone(),
        permission: tokio::sync::watch::channel(agent_harness::PermissionMode::Ask).1,
        before_file_tool: agent_harness::file_tools_go_ahead(),
        mcp: agent_harness::no_mcp_servers(),
    };
    let events = run_to_end(&harness(), request("scenario:askuser"), controls).await;

    let asked = asked.lock().unwrap();
    assert_eq!(asked.len(), 1);
    assert_eq!(asked[0].header, "Choice");
    assert_eq!(asked[0].question, "Pick one");
    assert_eq!(asked[0].options, vec!["A".to_string(), "B".to_string()]);
    assert!(
        !events.iter().any(|e| matches!(
            e,
            AgentEvent::InputRequested { .. } | AgentEvent::InputResolved { .. }
        )),
        "harness must not emit input lifecycle events itself: {events:?}"
    );

    assert_eq!(
        events.last(),
        Some(&AgentEvent::Done {
            status: DoneStatus::Completed,
            result: Some("answered".into()),
            error: None,
            session_id: Some("sess-ask".into()),
        })
    );
}

#[tokio::test]
async fn files_are_saved_before_claude_reads_them() {
    let cwd = tempfile::tempdir().expect("temporary directory");
    let mut req = request("scenario:hook");
    req.cwd = cwd.path().display().to_string();
    let (mut controls, _steer, _token) = controls("Allow");
    let saved = Arc::new(Mutex::new(Vec::new()));
    controls.before_file_tool = Arc::new({
        let saved = saved.clone();
        move |path| {
            saved.lock().unwrap().push(path);
            let (done_tx, done_rx) = oneshot::channel();
            done_tx.send(()).expect("receiver is still held");
            done_rx
        }
    });
    let events = run_to_end(&harness(), req, controls).await;

    assert_eq!(*saved.lock().unwrap(), vec!["/x/a.rs".to_string()]);
    // The command list from the initialize reply can arrive after the turn ends.
    assert_eq!(
        events
            .iter()
            .find(|event| matches!(event, AgentEvent::Done { .. })),
        Some(&AgentEvent::Done {
            status: DoneStatus::Completed,
            result: Some("read it".into()),
            error: None,
            session_id: Some("sess-hook".into()),
        })
    );
}

#[tokio::test]
async fn a_prompt_suggestion_follows_the_turn_when_asked_for() {
    let cwd = tempfile::tempdir().expect("temporary directory");
    let mut req = request("scenario:suggest");
    req.cwd = cwd.path().display().to_string();
    req.prompt_suggestions = true;
    let (controls, _steer, _token) = controls("Allow");
    let events = run_to_end(&harness(), req, controls).await;
    assert!(
        events.contains(&AgentEvent::PromptSuggestion {
            suggestion: "run the tests".into()
        }),
        "{events:?}"
    );
}

async fn mcp_reply(mcp: agent_harness::McpMessageFn) -> serde_json::Value {
    let cwd = tempfile::tempdir().expect("temporary directory");
    let mut req = request("scenario:mcp");
    req.cwd = cwd.path().display().to_string();
    let (mut controls, _steer, _token) = controls("Allow");
    controls.mcp = mcp;
    let events = run_to_end(&harness(), req, controls).await;
    assert!(
        events.iter().any(|event| matches!(
            event,
            AgentEvent::Done {
                status: DoneStatus::Completed,
                ..
            }
        )),
        "{events:?}"
    );
    let reply = std::fs::read_to_string(cwd.path().join(".mcp-reply")).expect("reply was written");
    let reply: serde_json::Value = serde_json::from_str(&reply).expect("reply is JSON");
    assert_eq!(reply["response"]["request_id"], "mcp-1");
    reply["response"]["response"]["mcp_response"].clone()
}

#[tokio::test]
async fn mcp_messages_are_answered_by_the_mcp_callback() {
    let received = Arc::new(Mutex::new(Vec::new()));
    let mcp: agent_harness::McpMessageFn = Arc::new({
        let received = received.clone();
        move |message| {
            received.lock().unwrap().push(message);
            let (reply_tx, reply_rx) = oneshot::channel();
            reply_tx
                .send(serde_json::json!({"jsonrpc": "2.0", "id": 1, "result": {"tools": []}}))
                .expect("receiver is still held");
            reply_rx
        }
    });

    let reply = mcp_reply(mcp).await;

    assert_eq!(
        *received.lock().unwrap(),
        vec![serde_json::json!({"method": "tools/list", "jsonrpc": "2.0", "id": 1})]
    );
    assert_eq!(
        reply,
        serde_json::json!({"jsonrpc": "2.0", "id": 1, "result": {"tools": []}})
    );
}

#[tokio::test]
async fn an_unanswered_mcp_message_gets_a_json_rpc_error() {
    let reply = mcp_reply(agent_harness::no_mcp_servers()).await;

    assert_eq!(reply["id"], 1);
    assert!(reply["error"]["message"].is_string(), "{reply}");
}

#[tokio::test]
async fn steering_lines_are_written_to_stdin_mid_run() {
    let (controls, steer, _token) = controls("A");
    steer
        .send(Steer::Message(SteerMessage {
            prompt: "redirect please".into(),
            message_id: None,
            attachments: Vec::new(),
            skills: Vec::new(),
            context: Vec::new(),
            settings: None,
        }))
        .await
        .expect("steer queued");
    let events = run_to_end(&harness(), request("scenario:steer"), controls).await;

    let steered = events
        .iter()
        .find_map(|e| match e {
            AgentEvent::Steered {
                assistant_message_id,
                next_assistant_message_id,
            } => Some((
                assistant_message_id.clone(),
                next_assistant_message_id.clone(),
            )),
            _ => None,
        })
        .expect("Steered emitted");
    let boundary = events
        .iter()
        .position(|e| matches!(e, AgentEvent::Steered { .. }))
        .unwrap();
    let continuation = events
        .iter()
        .position(|e| matches!(e, AgentEvent::TextDelta { text } if text == "-still-first"))
        .unwrap();
    assert!(
        continuation < boundary,
        "sending input must not split unconsumed response text"
    );
    assert!(steered.0.is_some() && steered.1.is_some());
    assert_ne!(steered.0, steered.1);

    assert!(events.contains(&AgentEvent::TextDelta {
        text: "steered:redirect please".into()
    }));
    assert!(matches!(
        events.last(),
        Some(AgentEvent::Done {
            status: DoneStatus::Completed,
            ..
        })
    ));
}

#[tokio::test]
async fn image_attachments_ride_the_prompt_and_steers_as_image_blocks() {
    let dir = tempfile::tempdir().unwrap();
    let png = dir.path().join("shot.png");
    let jpeg = dir.path().join("steer.JPG");
    std::fs::write(&png, "hello").unwrap();
    std::fs::write(&jpeg, "world").unwrap();
    let path = |p: &std::path::Path| p.to_string_lossy().into_owned();

    let (controls, steer, _token) = controls("A");
    steer
        .send(Steer::Message(SteerMessage {
            prompt: "and this one".into(),
            message_id: None,
            attachments: vec![path(&jpeg)],
            skills: Vec::new(),
            context: Vec::new(),
            settings: None,
        }))
        .await
        .expect("steer queued");
    let mut req = request("scenario:images");
    req.attachments = vec![path(&png), path(&dir.path().join("missing.png"))];
    let events = run_to_end(&harness(), req, controls).await;

    for text in ["first-image-ok", "steer-image-ok"] {
        assert!(
            events.contains(&AgentEvent::TextDelta { text: text.into() }),
            "{text}: {events:?}"
        );
    }
    assert!(matches!(
        events.last(),
        Some(AgentEvent::Done {
            status: DoneStatus::Completed,
            ..
        })
    ));
}

#[tokio::test]
async fn interrupt_escalates_to_sigterm_and_ends_with_interrupted_done() {
    let harness = ClaudeHarness::new()
        .with_executable(fixture_path())
        .with_graces(Duration::from_millis(100), Duration::from_millis(500));
    let (controls, _steer, token) = controls("A");
    let mut stream = harness
        .run(request("scenario:interrupt"), controls)
        .await
        .expect("run starts");

    let events = tokio::time::timeout(Duration::from_secs(10), async move {
        let mut events = Vec::new();
        while let Some(ev) = stream.next().await {
            let ev = ev.expect("stream event");
            if matches!(ev, AgentEvent::SessionStarted { .. }) {
                token.cancel();
            }
            events.push(ev);
        }
        events
    })
    .await
    .expect("interrupt completed in time");

    assert_eq!(
        events.last(),
        Some(&AgentEvent::Done {
            status: DoneStatus::Interrupted,
            result: None,
            error: None,
            session_id: Some("sess-int".into()),
        })
    );
}

#[tokio::test]
async fn error_codes_map_to_readable_messages() {
    let (controls, _steer, _token) = controls("A");
    let events = run_to_end(&harness(), request("scenario:error"), controls).await;

    let errors: Vec<&str> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::Error { message } => Some(message.as_str()),
            _ => None,
        })
        .collect();
    assert!(
        errors.contains(&"Claude usage limit reached. Try again after the limit resets."),
        "assistant error code not mapped: {errors:?}"
    );
    assert!(
        errors.contains(
            &"Claude 5-hour limit reached and the turn was blocked. Try again after it resets."
        ),
        "rejected rate_limit_event not mapped: {errors:?}"
    );

    assert_eq!(
        events.last(),
        Some(&AgentEvent::Done {
            status: DoneStatus::Errored,
            result: None,
            error: Some("The run hit the maximum number of turns.".into()),
            session_id: Some("sess-err".into()),
        })
    );
}

#[tokio::test]
async fn missing_binary_is_not_installed() {
    let harness = ClaudeHarness::new().with_executable("/nonexistent/claude-nowhere");
    let (controls, _steer, _token) = controls("A");
    let err = harness
        .run(request("scenario:happy"), controls)
        .await
        .err()
        .expect("spawn fails");
    assert!(matches!(err, HarnessError::NotInstalled(_)), "{err:?}");
}

fn write_replayer(directory: &std::path::Path, frames: &std::path::Path) -> PathBuf {
    #[cfg(windows)]
    {
        let cli = directory.join("replay.cmd");
        std::fs::write(&cli, format!("@type \"{}\"\r\n", frames.display()))
            .expect("replayer written");
        cli
    }
    #[cfg(not(windows))]
    {
        let cli = directory.join("replay.sh");
        std::fs::write(
            &cli,
            format!(
                "#!/bin/sh\nread -r _first || exit 1\ncat '{}'\n",
                frames.display()
            ),
        )
        .expect("replayer written");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        cli
    }
}

#[tokio::test]
async fn captured_live_text_deltas_stream_one_by_one() {
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("claude")
        .join("live-2.1.291-text-streaming.jsonl");
    let script = std::fs::read_to_string(&fixture).expect("fixture readable");
    let dir = std::env::temp_dir().join(format!("claude-stream-replay-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("tmp dir");
    let frames = dir.join("frames.jsonl");
    std::fs::write(&frames, &script).expect("frames written");
    let cli = write_replayer(&dir, &frames);

    let harness = ClaudeHarness::new().with_executable(&cli);
    let (controls, _steer, _token) = controls("A");
    let events = run_to_end(&harness, request("replay"), controls).await;
    let text_deltas = events
        .iter()
        .filter(|event| matches!(event, AgentEvent::TextDelta { .. }))
        .count();
    let source_deltas = script.matches("\"text_delta\"").count();
    assert!(source_deltas > 1);
    assert_eq!(text_deltas, source_deltas, "{events:?}");
}

#[tokio::test]
async fn captured_live_background_subagent_frames_replay_correctly() {
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("claude")
        .join("live-2.1.228-background-subagent.jsonl");
    let script = std::fs::read_to_string(&fixture).expect("fixture readable");
    let dir = std::env::temp_dir().join(format!("claude-replay-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("tmp dir");
    let frames = dir.join("frames.jsonl");
    std::fs::write(&frames, &script).expect("frames written");
    let cli = write_replayer(&dir, &frames);

    let harness = ClaudeHarness::new().with_executable(&cli);
    let (controls, _steer, _token) = controls("A");
    let events = run_to_end(&harness, request("replay"), controls).await;

    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, AgentEvent::SessionStarted { .. }))
            .count(),
        1,
        "{events:?}"
    );
    let dones: Vec<usize> = events
        .iter()
        .enumerate()
        .filter_map(|(i, e)| matches!(e, AgentEvent::Done { .. }).then_some(i))
        .collect();
    assert_eq!(dones.len(), 2, "eager done + wake done: {events:?}");

    let spawn_id = events
        .iter()
        .find_map(|e| match e {
            AgentEvent::ToolCall { id, call } => match call {
                ToolCall::Unknown { name, .. } if name.starts_with("Agent") => Some(id.clone()),
                _ => None,
            },
            _ => None,
        })
        .expect("Agent spawn tool call in the parent feed");
    let opening: Vec<usize> = events
        .iter()
        .enumerate()
        .filter_map(|(i, e)| {
            matches!(
                e,
                AgentEvent::Subagent { parent_tool_use_id, event }
                    if *parent_tool_use_id == spawn_id
                        && matches!(event.as_ref(), AgentEvent::UserMessage { .. })
            )
            .then_some(i)
        })
        .collect();
    assert_eq!(opening.len(), 1, "one seeded opening prompt: {events:?}");
    assert!(opening[0] < dones[0], "opening rides with the spawn");
    let tagged: Vec<usize> = events
        .iter()
        .enumerate()
        .filter_map(|(i, e)| {
            matches!(
                e,
                AgentEvent::Subagent { parent_tool_use_id, event }
                    if *parent_tool_use_id == spawn_id
                        && !matches!(event.as_ref(), AgentEvent::UserMessage { .. })
            )
            .then_some(i)
        })
        .collect();
    assert!(!tagged.is_empty(), "tagged subagent traffic: {events:?}");
    assert!(
        tagged.iter().all(|i| dones[0] < *i && *i < dones[1]),
        "subagent interior streams between the eager and wake dones"
    );
    assert!(
        events.iter().any(|e| matches!(
            e,
            AgentEvent::Subagent { event, .. }
                if matches!(event.as_ref(), AgentEvent::ToolCall { call: ToolCall::Exec { .. }, .. })
        )),
        "subagent Bash call arrives tagged: {events:?}"
    );

    if let Err(error) = std::fs::remove_dir_all(&dir) {
        eprintln!("could not remove {}: {error}", dir.display());
    }
}

#[tokio::test]
#[ignore = "spawns the real claude CLI; needs install + auth + network"]
async fn live_real_cli_single_turn() {
    let harness = ClaudeHarness::new();
    let mut req = request("Reply with exactly the word: pong");
    req.model = Some("haiku".into());
    req.cwd = std::env::temp_dir().display().to_string();
    req.permission = PermissionMode::Auto;
    let (controls, _steer, _token) = controls("A");
    let mut stream = harness.run(req, controls).await.expect("run starts");
    let events = tokio::time::timeout(Duration::from_secs(120), async {
        let mut events = Vec::new();
        while let Some(ev) = stream.next().await {
            let ev = ev.expect("stream event");
            let done = matches!(ev, AgentEvent::Done { .. });
            events.push(ev);
            if done {
                break;
            }
        }
        events
    })
    .await
    .expect("live turn finished in time");
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::SessionStarted { .. })),
        "{events:?}"
    );
    assert!(matches!(
        events.last(),
        Some(AgentEvent::Done {
            status: DoneStatus::Completed,
            ..
        })
    ));
}

#[tokio::test]
async fn a_replay_confirms_superseded_steers_and_the_turn_ends() {
    let (controls, steer, _token) = controls("A");
    for prompt in ["first steer", "second steer"] {
        steer
            .send(Steer::Message(SteerMessage {
                prompt: prompt.into(),
                message_id: None,
                attachments: Vec::new(),
                skills: Vec::new(),
                context: Vec::new(),
                settings: None,
            }))
            .await
            .expect("steer queued");
    }
    let events = tokio::time::timeout(
        std::time::Duration::from_secs(20),
        run_to_end(&harness(), request("scenario:superseded-steers"), controls),
    )
    .await
    .expect("run must end");
    let steered = events
        .iter()
        .filter(|e| matches!(e, AgentEvent::Steered { .. }))
        .count();
    assert_eq!(steered, 2, "{events:?}");
    let dones: Vec<_> = events
        .iter()
        .filter(|e| matches!(e, AgentEvent::Done { .. }))
        .collect();
    assert_eq!(dones.len(), 1, "{events:?}");
    assert!(events.contains(&AgentEvent::TextDelta {
        text: "answered-both".into()
    }));
    assert!(matches!(
        events.last(),
        Some(AgentEvent::Done {
            status: DoneStatus::Completed,
            ..
        })
    ));
}

#[tokio::test]
async fn an_unreplayed_steer_releases_the_turn_end() {
    let (controls, steer, _token) = controls("A");
    steer
        .send(Steer::Message(SteerMessage {
            prompt: "absorbed steer".into(),
            message_id: None,
            attachments: Vec::new(),
            skills: Vec::new(),
            context: Vec::new(),
            settings: None,
        }))
        .await
        .expect("steer queued");
    let started = std::time::Instant::now();
    let mut stream = harness()
        .run(request("scenario:absorbed-steer"), controls)
        .await
        .expect("run starts");
    let mut steered = 0;
    let done = tokio::time::timeout(std::time::Duration::from_secs(15), async {
        while let Some(event) = stream.next().await {
            match event.expect("event") {
                AgentEvent::Steered { .. } => steered += 1,
                AgentEvent::Done { status, .. } => return status,
                _ => {}
            }
        }
        panic!("stream ended without Done");
    })
    .await
    .expect("turn end must be released");
    assert_eq!(done, DoneStatus::Completed);
    assert_eq!(steered, 1);
    assert!(started.elapsed() >= std::time::Duration::from_secs(4));
}

fn permission_controls(
    permission_label: &'static str,
    asked: Arc<Mutex<Vec<UserInputQuestion>>>,
) -> (RunControls, mpsc::Sender<Steer>) {
    let (steer_tx, steer_rx) = mpsc::channel(8);
    let controls = RunControls {
        request_input: Box::new(move |questions| {
            asked.lock().unwrap().extend(questions.iter().cloned());
            let (tx, rx) = oneshot::channel();
            let answers: Vec<UserInputAnswer> = questions
                .iter()
                .map(|q| UserInputAnswer {
                    question_id: q.id.clone(),
                    labels: vec![if q.header == agent_harness::claude::PERMISSION_HEADER {
                        permission_label.into()
                    } else {
                        "B".into()
                    }],
                    decision: None,
                })
                .collect();
            tx.send(answers).ok();
            rx
        }),
        steering: steer_rx,
        interrupt: CancellationToken::new(),
        permission: tokio::sync::watch::channel(agent_harness::PermissionMode::Ask).1,
        before_file_tool: agent_harness::file_tools_go_ahead(),
        mcp: agent_harness::no_mcp_servers(),
    };
    (controls, steer_tx)
}

#[tokio::test]
async fn tool_permissions_are_asked_when_not_auto_approving() {
    let asked = Arc::new(Mutex::new(Vec::new()));
    let mut req = request("scenario:askuser");
    req.permission = PermissionMode::Auto;
    let (controls, _steer) = permission_controls("Allow", asked.clone());
    let events = run_to_end(&harness(), req, controls).await;

    let asked = asked.lock().unwrap();
    assert_eq!(
        asked.len(),
        2,
        "permission, then the agent's own question: {asked:?}"
    );
    assert_eq!(asked[0].header, agent_harness::claude::PERMISSION_HEADER);
    assert_eq!(asked[0].question, "Run `ls`?");
    assert_eq!(
        asked[0].options,
        vec!["Allow".to_string(), "Deny".to_string()]
    );
    assert_eq!(asked[1].question, "Pick one");
    assert!(matches!(
        events.last(),
        Some(AgentEvent::Done {
            status: DoneStatus::Completed,
            ..
        })
    ));
}

#[tokio::test]
async fn denied_tool_permissions_reach_the_cli_as_deny() {
    let asked = Arc::new(Mutex::new(Vec::new()));
    let mut req = request("scenario:askuser");
    req.permission = PermissionMode::Auto;
    let (controls, _steer) = permission_controls("Deny", asked.clone());
    let events = run_to_end(&harness(), req, controls).await;

    assert_eq!(asked.lock().unwrap().len(), 1);
    assert!(
        matches!(
            events.last(),
            Some(AgentEvent::Done {
                status: DoneStatus::Errored,
                ..
            })
        ),
        "the fixture fails the turn when Bash is not allowed: {events:?}"
    );
}

#[tokio::test]
async fn slash_commands_come_from_initialize_in_the_chat_folder() {
    let cwd = tempfile::tempdir().expect("temporary directory");
    let commands = harness().commands(cwd.path()).await.expect("commands");
    let names: Vec<&str> = commands
        .iter()
        .map(|command| command.name.as_str())
        .collect();
    assert_eq!(names, ["review", "compact"]);
    assert_eq!(commands[0].description, "Review a pull request");
    assert_eq!(commands[0].input_hint.as_deref(), Some("[pr number]"));
    assert_eq!(commands[1].input_hint, None);

    let own_command = cwd.path().join(".claude/commands/usage.md");
    std::fs::create_dir_all(own_command.parent().expect("parent")).expect("create directory");
    std::fs::write(&own_command, "My own usage report").expect("write command");
    let commands = harness().commands(cwd.path()).await.expect("commands");
    let names: Vec<&str> = commands
        .iter()
        .map(|command| command.name.as_str())
        .collect();
    assert_eq!(names, ["review", "compact", "usage"]);

    std::fs::write(cwd.path().join(".command-fixture"), "project-only").expect("write fixture");
    let commands = harness().commands(cwd.path()).await.expect("commands");
    assert_eq!(commands.len(), 1);
    assert_eq!(commands[0].name, "project-only");
}

fn stop() -> Steer {
    Steer::StopTurn(oneshot::channel().0)
}

fn message(prompt: &str) -> Steer {
    Steer::Message(SteerMessage {
        prompt: prompt.into(),
        message_id: None,
        attachments: Vec::new(),
        skills: Vec::new(),
        context: Vec::new(),
        settings: None,
    })
}

type EventStream = futures::stream::BoxStream<'static, Result<AgentEvent, HarnessError>>;

async fn events_through(
    stream: &mut EventStream,
    last: impl Fn(&AgentEvent) -> bool,
) -> Vec<AgentEvent> {
    tokio::time::timeout(Duration::from_secs(10), async {
        let mut events = Vec::new();
        while let Some(event) = stream.next().await {
            let event = event.expect("stream event");
            let found = last(&event);
            events.push(event);
            if found {
                break;
            }
        }
        events
    })
    .await
    .expect("the awaited event arrived in time")
}

async fn turn_events(stream: &mut EventStream) -> Vec<AgentEvent> {
    events_through(stream, |event| matches!(event, AgentEvent::Done { .. })).await
}

fn reply_text(events: &[AgentEvent]) -> String {
    events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::TextDelta { text } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

fn done_statuses(events: &[AgentEvent]) -> Vec<DoneStatus> {
    events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::Done { status, .. } => Some(*status),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn a_stopped_turn_ends_and_the_same_process_answers_the_next_message() {
    let harness = harness().with_graces(Duration::from_secs(8), Duration::from_secs(1));
    assert!(harness.stops_turn_in_place());
    let (controls, steer, _token) = controls("A");
    let mut stream = harness
        .run(request("scenario:stop"), controls)
        .await
        .expect("run starts");

    let first = events_through(&mut stream, |event| {
        matches!(event, AgentEvent::TextDelta { .. })
    })
    .await;
    let first_text = reply_text(&first);
    let process = first_text
        .strip_prefix("turn 1 in ")
        .expect("the fake reports its process id");

    let (acknowledge, mut acknowledged) = oneshot::channel();
    steer
        .send(Steer::StopTurn(acknowledge))
        .await
        .expect("stop queued");
    steer
        .send(message("after the stop"))
        .await
        .expect("message queued");

    let stopped = turn_events(&mut stream).await;
    assert_eq!(done_statuses(&stopped), [DoneStatus::Interrupted]);
    assert!(
        acknowledged.try_recv().is_ok(),
        "the stop is confirmed first"
    );
    assert_eq!(reply_text(&stopped), "", "{stopped:?}");

    let second = turn_events(&mut stream).await;
    assert_eq!(done_statuses(&second), [DoneStatus::Completed]);
    assert_eq!(
        reply_text(&second),
        format!("turn 2 in {process}: after the stop")
    );

    let idle = tokio::time::timeout(Duration::from_millis(300), stream.next()).await;
    assert!(idle.is_err(), "the run stays open after a stop: {idle:?}");
}

#[tokio::test]
async fn a_stop_withdraws_the_question_the_turn_was_waiting_on() {
    let waiting = Arc::new(Mutex::new(Vec::new()));
    let (steer, steer_rx) = mpsc::channel(8);
    let controls = RunControls {
        request_input: Box::new({
            let waiting = waiting.clone();
            move |_questions| {
                let (answer_tx, answer_rx) = oneshot::channel();
                waiting.lock().unwrap().push(answer_tx);
                answer_rx
            }
        }),
        steering: steer_rx,
        interrupt: CancellationToken::new(),
        permission: tokio::sync::watch::channel(PermissionMode::Ask).1,
        before_file_tool: agent_harness::file_tools_go_ahead(),
        mcp: agent_harness::no_mcp_servers(),
    };
    let mut req = request("scenario:stop-asking");
    req.permission = PermissionMode::Ask;
    let mut stream = harness()
        .with_graces(Duration::from_secs(8), Duration::from_secs(1))
        .run(req, controls)
        .await
        .expect("run starts");
    events_through(&mut stream, |event| {
        matches!(event, AgentEvent::TextDelta { .. })
    })
    .await;
    tokio::time::timeout(Duration::from_secs(5), async {
        while waiting.lock().unwrap().is_empty() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the turn asked its question");

    steer.send(stop()).await.expect("stop queued");
    let stopped = turn_events(&mut stream).await;
    assert_eq!(done_statuses(&stopped), [DoneStatus::Interrupted]);
    waiting.lock().unwrap().clear();

    steer.send(message("after")).await.expect("message queued");
    let second = turn_events(&mut stream).await;
    assert!(reply_text(&second).starts_with("turn 2 in "), "{second:?}");
    let late = tokio::time::timeout(Duration::from_millis(300), stream.next()).await;
    assert!(
        late.is_err(),
        "nothing answers the withdrawn question: {late:?}"
    );
}

#[tokio::test]
async fn a_stop_between_turns_is_ignored() {
    let (controls, steer, _token) = controls("A");
    let mut stream = harness()
        .run(request("scenario:stop-idle"), controls)
        .await
        .expect("run starts");
    let first = turn_events(&mut stream).await;
    assert_eq!(done_statuses(&first), [DoneStatus::Completed]);

    steer.send(stop()).await.expect("stop queued");
    steer.send(message("next")).await.expect("message queued");
    let second = turn_events(&mut stream).await;
    assert_eq!(done_statuses(&second), [DoneStatus::Completed]);
    assert!(reply_text(&second).ends_with(": next"), "{second:?}");
    assert!(
        !second
            .iter()
            .any(|event| matches!(event, AgentEvent::Error { .. })),
        "{second:?}"
    );
}

#[tokio::test]
async fn a_stop_the_agent_ignores_ends_the_run() {
    let harness = ClaudeHarness::new()
        .with_executable(fixture_path())
        .with_graces(Duration::from_millis(100), Duration::from_millis(500));
    let (controls, steer, _token) = controls("A");
    let mut stream = harness
        .run(request("scenario:interrupt"), controls)
        .await
        .expect("run starts");

    let events = tokio::time::timeout(Duration::from_secs(10), async move {
        let mut events = Vec::new();
        while let Some(event) = stream.next().await {
            let event = event.expect("stream event");
            if matches!(event, AgentEvent::SessionStarted { .. }) {
                steer.send(stop()).await.expect("stop queued");
            }
            events.push(event);
        }
        events
    })
    .await
    .expect("the run ended in time");

    assert_eq!(done_statuses(&events), [DoneStatus::Interrupted]);
    assert_eq!(
        events.last(),
        Some(&AgentEvent::Done {
            status: DoneStatus::Interrupted,
            result: None,
            error: None,
            session_id: Some("sess-int".into()),
        })
    );
}

fn turn_settings(model: &str, options: serde_json::Value) -> agent_harness::TurnSettings {
    agent_harness::TurnSettings {
        model: Some(model.into()),
        reasoning: None,
        model_options: options.as_object().cloned().unwrap_or_default(),
    }
}

#[tokio::test]
async fn a_model_change_reaches_the_running_agent_without_a_restart() {
    let harness = harness();
    let alpha = turn_settings("gateway/alpha", serde_json::json!({}));
    let beta = turn_settings("gateway/beta", serde_json::json!({ "contextWindow": "1m" }));
    assert!(harness.applies_settings_live(&alpha, &beta));
    let mut slower = beta.clone();
    slower.reasoning = Some(agent_harness::ReasoningLevel::Low);
    assert!(!harness.applies_settings_live(&beta, &slower));
    let fast = turn_settings("gateway/beta", serde_json::json!({ "fastMode": "on" }));
    assert!(!harness.applies_settings_live(&beta, &fast));
    let mut default_model = beta.clone();
    default_model.model = None;
    assert!(!harness.applies_settings_live(&beta, &default_model));

    let mut req = request("scenario:models");
    req.model = alpha.model.clone();
    let (controls, steer, _token) = controls("A");
    let mut stream = harness.run(req, controls).await.expect("run starts");
    let first = turn_events(&mut stream).await;
    assert_eq!(reply_text(&first), "model=gateway/alpha switches=0");

    let with_settings = |settings: agent_harness::TurnSettings| {
        Steer::Message(SteerMessage {
            prompt: "again".into(),
            message_id: None,
            attachments: Vec::new(),
            skills: Vec::new(),
            context: Vec::new(),
            settings: Some(settings),
        })
    };
    steer
        .send(with_settings(beta.clone()))
        .await
        .expect("queued");
    let switched = turn_events(&mut stream).await;
    assert_eq!(reply_text(&switched), "model=gateway/beta[1m] switches=1");

    for next in [message("again"), with_settings(beta)] {
        steer.send(next).await.expect("queued");
        let kept = turn_events(&mut stream).await;
        assert_eq!(reply_text(&kept), "model=gateway/beta[1m] switches=1");
    }

    steer
        .send(with_settings(turn_settings(
            "gateway/unknown",
            serde_json::json!({}),
        )))
        .await
        .expect("queued");
    let refused = turn_events(&mut stream).await;
    assert!(
        refused.iter().any(
            |event| matches!(event, AgentEvent::Error { message } if message.ends_with("no such model"))
        ),
        "{refused:?}"
    );
    assert_eq!(reply_text(&refused), "model=gateway/beta[1m] switches=1");
}

#[tokio::test]
async fn thinking_toggle_is_applied_live() {
    let harness = harness();
    let on = turn_settings("gateway/alpha", serde_json::json!({ "thinking": "on" }));
    let off = turn_settings("gateway/alpha", serde_json::json!({ "thinking": "off" }));
    assert!(harness.applies_settings_live(&on, &off));

    let mut req = request("scenario:thinking");
    req.model = off.model.clone();
    req.model_options = off.model_options.clone();
    let (controls, steer, _token) = controls("A");
    let mut stream = harness.run(req, controls).await.expect("run starts");
    let first = turn_events(&mut stream).await;
    assert_eq!(reply_text(&first), "launch=disabled last=none");

    let with_settings = |settings: &agent_harness::TurnSettings| {
        Steer::Message(SteerMessage {
            prompt: "again".into(),
            message_id: None,
            attachments: Vec::new(),
            skills: Vec::new(),
            context: Vec::new(),
            settings: Some(settings.clone()),
        })
    };
    for (settings, expected) in [
        (&on, "launch=disabled last=31999/summarized"),
        (&on, "launch=disabled last=31999/summarized"),
        (&off, "launch=disabled last=0/null"),
    ] {
        steer.send(with_settings(settings)).await.expect("queued");
        let turn = turn_events(&mut stream).await;
        assert_eq!(reply_text(&turn), expected);
    }
}

#[tokio::test]
async fn a_run_reports_the_same_commands_as_the_lookup() {
    let cwd = tempfile::tempdir().expect("temporary directory");
    let own_command = cwd.path().join(".claude/commands/usage.md");
    std::fs::create_dir_all(own_command.parent().expect("parent")).expect("create directory");
    std::fs::write(&own_command, "My own usage report").expect("write command");
    let harness = harness();
    let looked_up = harness.commands(cwd.path()).await.expect("commands");
    assert_eq!(looked_up.len(), 3);

    let mut req = request("scenario:commands");
    req.cwd = cwd.path().display().to_string();
    let events = run_to_end(&harness, req, controls("A").0).await;
    let reported: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::AvailableCommands { commands } => Some(commands.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(reported, [looked_up]);
    assert_eq!(done_statuses(&events), [DoneStatus::Completed]);
}

async fn control(
    steer: &mpsc::Sender<Steer>,
    request: serde_json::Value,
) -> oneshot::Receiver<Result<serde_json::Value, String>> {
    let (reply, answer) = oneshot::channel();
    steer
        .send(Steer::Control(ControlCall { request, reply }))
        .await
        .expect("control queued");
    answer
}

#[tokio::test]
async fn control_call_round_trips() {
    let (controls, steer, _token) = controls("A");
    let mut stream = harness()
        .run(request("scenario:control"), controls)
        .await
        .expect("run starts");
    turn_events(&mut stream).await;

    let answer = control(&steer, serde_json::json!({ "subtype": "get_plan" })).await;
    let reply = tokio::time::timeout(Duration::from_secs(10), answer)
        .await
        .expect("answered in time")
        .expect("the run kept the reply")
        .expect("the CLI answered with success");
    assert_eq!(reply["exists"], true);
}

#[tokio::test]
async fn control_error_and_timeout_surface() {
    let (controls, steer, _token) = controls("A");
    let mut stream = harness()
        .run(request("scenario:control"), controls)
        .await
        .expect("run starts");
    turn_events(&mut stream).await;

    let refused = control(&steer, serde_json::json!({ "subtype": "mcp_status" })).await;
    let refused = tokio::time::timeout(Duration::from_secs(10), refused)
        .await
        .expect("answered in time")
        .expect("the run kept the reply");
    assert!(
        refused
            .as_ref()
            .is_err_and(|error| error.contains("MCP servers are unavailable")),
        "{refused:?}"
    );

    let unanswered = control(&steer, serde_json::json!({ "subtype": "hang" })).await;
    drop(steer);
    let ended = tokio::time::timeout(Duration::from_secs(10), unanswered)
        .await
        .expect("the run ending resolved the call");
    assert!(!matches!(ended, Ok(Ok(_))), "{ended:?}");
}

#[tokio::test]
async fn login_flow_sends_three_requests_to_one_process() {
    let (urls_sender, urls) = oneshot::channel();
    let (codes, pasted) = mpsc::channel(1);
    let login = tokio::spawn(async move {
        harness()
            .login(std::path::Path::new(""), urls_sender, pasted)
            .await
    });
    let urls = tokio::time::timeout(Duration::from_secs(10), urls)
        .await
        .expect("sign-in addresses in time")
        .expect("the login sent its addresses");
    assert_eq!(
        urls.automatic.as_deref(),
        Some("https://claude.example/automatic?state=from-url")
    );
    codes
        .send("abc#xyz".into())
        .await
        .expect("login reads codes");
    let signed_in = tokio::time::timeout(Duration::from_secs(10), login)
        .await
        .expect("signed in in time")
        .expect("login task ran")
        .expect("the CLI completed the sign-in");
    assert_eq!(signed_in["account"]["email"], "me@example.com");
    assert_eq!(signed_in["callback"], "abc|xyz");
}

#[tokio::test]
async fn a_cut_off_turn_continues_without_a_prompt() {
    let mut request = request("");
    request.resume = Some("sess-cut-off".into());
    request.resume_interrupted = true;
    let (controls, _steer, _token) = controls("A");
    let events = run_to_end(&harness(), request, controls).await;
    assert!(
        events.iter().any(|event| matches!(
            event,
            AgentEvent::TextDelta { text } if text == "continued within 600000 ms"
        )),
        "{events:?}"
    );
    assert!(
        matches!(
            events.last(),
            Some(AgentEvent::Done {
                status: DoneStatus::Completed,
                ..
            })
        ),
        "{events:?}"
    );
}

#[tokio::test]
async fn a_resume_with_nothing_to_continue_ends_quietly() {
    let mut request = request("");
    request.resume = Some("sess-finished".into());
    request.resume_interrupted = true;
    let harness = harness().with_resumed_turn_timeout(Duration::from_millis(500));
    let (controls, _steer, _token) = controls("A");
    let events = run_to_end(&harness, request, controls).await;
    assert!(
        matches!(
            events.as_slice(),
            [AgentEvent::Done {
                status: DoneStatus::Interrupted,
                error: None,
                ..
            }]
        ),
        "{events:?}"
    );
}

#[tokio::test]
async fn a_slow_start_of_a_continued_turn_is_not_given_up_on() {
    let mut request = request("");
    request.resume = Some("sess-slow-start".into());
    request.resume_interrupted = true;
    // The startup frames take about 900 ms in all, longer than the timeout, but none is 500 ms apart from the next.
    let harness = harness().with_resumed_turn_timeout(Duration::from_millis(500));
    let (controls, _steer, _token) = controls("A");
    let events = run_to_end(&harness, request, controls).await;
    assert!(
        matches!(
            events.last(),
            Some(AgentEvent::Done {
                status: DoneStatus::Completed,
                ..
            })
        ),
        "{events:?}"
    );
}
