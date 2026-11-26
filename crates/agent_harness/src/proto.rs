use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HarnessId {
    ClaudeCode,
    Codex,
    Opencode,
    Antigravity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReasoningLevel {
    Minimal,
    Low,
    Medium,
    High,
    XHigh,
    Max,
    Ultra,
    /// xhigh plus a harness-specific setting.
    Ultracode,
    /// Driven by a prompt prefix.
    Ultrathink,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SandboxLevel {
    ReadOnly,
    WorkspaceWrite,
    DangerFullAccess,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionMode {
    Ask,
    Plan,
    AcceptEdits,
    #[default]
    Auto,
    DontAsk,
    ReadOnly,
    ApproveForMe,
    FullAccess,
}

impl PermissionMode {
    pub fn choices(harness: HarnessId) -> &'static [PermissionMode] {
        match harness {
            HarnessId::ClaudeCode => &[
                Self::Ask,
                Self::Plan,
                Self::AcceptEdits,
                Self::Auto,
                Self::DontAsk,
                Self::FullAccess,
            ],
            HarnessId::Codex => &[
                Self::ReadOnly,
                Self::Auto,
                Self::ApproveForMe,
                Self::FullAccess,
            ],
            HarnessId::Opencode | HarnessId::Antigravity => {
                &[Self::Ask, Self::AcceptEdits, Self::Auto, Self::FullAccess]
            }
        }
    }

    /// Falls back to `Auto` for modes the harness doesn't offer.
    pub fn for_harness(self, harness: HarnessId) -> Self {
        if Self::choices(harness).contains(&self) {
            self
        } else {
            Self::Auto
        }
    }

    pub fn skips_prompts(self) -> bool {
        self == Self::FullAccess
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SteeringMode {
    StepBoundary,
    TurnBoundary,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Model {
    pub id: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default)]
    pub reasoning_levels: Vec<ReasoningLevel>,
    #[serde(default)]
    pub options: Vec<ModelOption>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelOption {
    pub id: String,
    pub label: String,
    pub choices: Vec<ModelOptionChoice>,
    pub default_choice: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelOptionChoice {
    pub id: String,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunRequest {
    pub prompt: String,
    pub model: Option<String>,
    pub reasoning: Option<ReasoningLevel>,
    /// Option id to choice id.
    #[serde(default)]
    pub model_options: serde_json::Map<String, serde_json::Value>,
    pub cwd: String,
    #[serde(default)]
    pub permission: PermissionMode,
    /// Harness-native session id.
    pub resume: Option<String>,
    /// Harness-native session id to copy into a new session; ignored when `resume` is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fork: Option<String>,
    /// Absolute image file paths.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skills: Vec<SkillRef>,
    /// Context size in tokens at which the agent compacts; `None` keeps the agent's own default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_compact_tokens: Option<u64>,
    /// Lets Claude Code use the browser through the Claude in Chrome extension.
    #[serde(default)]
    pub chrome: bool,
    /// A `PromptRecorded` id: the agent forgets that prompt and everything after it before this run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drop_prompt: Option<String>,
    /// The agent doesn't keep this run as a saved chat.
    #[serde(default)]
    pub ephemeral: bool,
    /// Text blocks sent ahead of the prompt, such as the editor selection. Agents without a way to send them ignore them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub context: Vec<String>,
    /// Runs again the turn that `resume`'s session was in the middle of when it was cut off, instead of sending `prompt`. Only Claude Code supports it.
    #[serde(default)]
    pub resume_interrupted: bool,
    /// Asks the agent to suggest the next prompt after each turn. Only Claude Code supports it.
    #[serde(default)]
    pub prompt_suggestions: bool,
    /// How Claude Code is started, from the user's settings. Other agents ignore it.
    #[serde(default, skip_serializing_if = "ClaudeLaunch::is_default")]
    pub launch: ClaudeLaunch,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeLaunch {
    /// Added to the process environment after Wu's own, so they win, except `CLAUDE_CODE_SSE_PORT` and `CLAUDE_CODE_RESUME_*`, which Wu controls for safety.
    #[serde(default)]
    pub environment: Vec<(String, String)>,
    /// Started instead of `claude`, with the path of `claude` as its first argument.
    #[serde(default)]
    pub process_wrapper: Option<String>,
    /// Verbs the spinner shows while Claude Code works.
    #[serde(default)]
    pub spinner_verbs: Vec<String>,
    /// Whether `spinner_verbs` replace Claude Code's own instead of adding to them.
    #[serde(default)]
    pub replace_spinner_verbs: bool,
}

static CURRENT_CLAUDE_LAUNCH: std::sync::RwLock<ClaudeLaunch> =
    std::sync::RwLock::new(ClaudeLaunch {
        environment: Vec::new(),
        process_wrapper: None,
        spinner_verbs: Vec::new(),
        replace_spinner_verbs: false,
    });

impl ClaudeLaunch {
    fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// Claude Code processes that aren't chat runs (control requests, model discovery, sign-in) have no `RunRequest` to carry these, so they use the ones last set here.
    pub fn set_current(self) {
        *CURRENT_CLAUDE_LAUNCH
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = self;
    }

    pub fn current() -> Self {
        CURRENT_CLAUDE_LAUNCH
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

/// How long after a chat was cut off mid-turn its turn may still run again.
pub const INTERRUPTED_TURN_MAX_AGE: std::time::Duration = std::time::Duration::from_secs(10 * 60);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExternalSession {
    pub id: String,
    pub title: String,
    /// Unix seconds.
    pub updated_at: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub path: Option<String>,
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillRef {
    pub name: String,
    pub path: String,
}

/// Reappears in every segment of a run, so tool-id de-duplication must exempt it.
pub const LIVE_PLAN_TOOL_ID: &str = "acp-plan";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ToolCall {
    Exec {
        command: String,
    },
    ReadFile {
        path: String,
    },
    WriteFile {
        path: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        content: Option<String>,
    },
    EditFile {
        path: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        old_string: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        new_string: Option<String>,
    },
    ApplyPatch {
        #[serde(skip_serializing_if = "Option::is_none")]
        path: Option<String>,
    },
    Search {
        pattern: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        path: Option<String>,
    },
    Glob {
        pattern: String,
    },
    WebFetch {
        url: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        prompt: Option<String>,
    },
    WebSearch {
        query: String,
    },
    Todo {
        #[serde(default)]
        items: Vec<TodoItem>,
    },
    Mcp {
        server: String,
        tool: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        input: Option<serde_json::Value>,
    },
    Unknown {
        name: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        input: Option<serde_json::Value>,
    },
}

impl ToolCall {
    pub fn is_subagent_spawn(&self) -> bool {
        let name = match self {
            ToolCall::Unknown { name, .. } => name,
            ToolCall::Mcp { tool, .. } => tool,
            _ => return false,
        };
        name == "Agent" || name.starts_with("Agent: ")
    }

    pub fn subagent_model(&self) -> Option<&str> {
        if !self.is_subagent_spawn() {
            return None;
        }
        let input = match self {
            ToolCall::Unknown { input, .. } | ToolCall::Mcp { input, .. } => input.as_ref()?,
            _ => return None,
        };
        SUBAGENT_MODEL_KEYS
            .iter()
            .find_map(|key| input.get(key).and_then(serde_json::Value::as_str))
            .map(str::trim)
            .filter(|model| !model.is_empty())
    }
}

/// In precedence order.
pub const SUBAGENT_MODEL_KEYS: [&str; 4] = ["model", "modelId", "model_id", "subagent_model"];

pub const SUBAGENT_INPUT_KEEP: [&str; 5] = [
    "model",
    "modelId",
    "model_id",
    "subagent_model",
    "subagent_type",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TodoStatus {
    #[default]
    Pending,
    InProgress,
    Completed,
}

impl TodoStatus {
    pub fn parse(raw: &str) -> Self {
        match raw {
            "completed" | "complete" | "done" => Self::Completed,
            "in_progress" | "inProgress" | "in-progress" | "active" => Self::InProgress,
            _ => Self::Pending,
        }
    }
}

/// `done` stays authoritative for completion; build with [`TodoItem::new`] to keep both consistent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TodoItem {
    pub text: String,
    pub done: bool,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient_todo_status"
    )]
    pub status: Option<TodoStatus>,
}

impl TodoItem {
    pub fn new(text: impl Into<String>, status: TodoStatus) -> Self {
        Self {
            text: text.into(),
            done: status == TodoStatus::Completed,
            status: (status == TodoStatus::InProgress).then_some(status),
        }
    }

    pub fn status(&self) -> TodoStatus {
        if self.done {
            TodoStatus::Completed
        } else {
            self.status.unwrap_or_default()
        }
    }
}

// An unknown status must not fail the whole tool call.
fn lenient_todo_status<'de, D>(deserializer: D) -> Result<Option<TodoStatus>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;
    Ok(value.and_then(|v| serde_json::from_value(v).ok()))
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SlashCommand {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_hint: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_skill: bool,
}

/// `old_text: None` means a new file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolDiff {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_text: Option<String>,
    pub new_text: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserInputQuestion {
    pub id: String,
    pub header: String,
    pub question: String,
    pub options: Vec<String>,
    #[serde(default)]
    pub multi_select: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefill: Option<String>,
    #[serde(default)]
    pub multiline: bool,
    #[serde(default)]
    pub accepts_typed_answer: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission: Option<PermissionPrompt>,
}

/// What the agent asks permission for, as the CLI's `can_use_tool` request describes it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct PermissionPrompt {
    pub tool_name: String,
    pub input: serde_json::Value,
    pub tool_use_id: Option<String>,
    pub suggestions: Vec<serde_json::Value>,
    pub allows_always: bool,
    pub blocked_path: Option<String>,
    pub title: Option<String>,
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "behavior", rename_all = "camelCase")]
pub enum PermissionDecision {
    #[serde(rename_all = "camelCase")]
    Allow {
        #[serde(default)]
        updated_input: Option<serde_json::Value>,
        #[serde(default)]
        updated_permissions: Vec<serde_json::Value>,
    },
    #[serde(rename_all = "camelCase")]
    Deny {
        #[serde(default)]
        message: String,
        #[serde(default)]
        interrupt: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserInputAnswer {
    pub question_id: String,
    pub labels: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision: Option<PermissionDecision>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DoneStatus {
    Completed,
    Interrupted,
    Errored,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BackgroundTaskKind {
    Shell,
    Agent,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackgroundTaskInfo {
    pub task_id: String,
    pub kind: BackgroundTaskKind,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum AgentEvent {
    #[serde(rename_all = "camelCase")]
    SessionStarted {
        harness: HarnessId,
        model: String,
        #[serde(default)]
        tools: Vec<String>,
        cwd: String,
        session_id: String,
        assistant_message_id: String,
        /// Protocol features the agent announced, e.g. Claude Code's `ui_surface_v1` for plugin drawing.
        #[serde(default)]
        capabilities: Vec<String>,
    },
    /// A plugin drawing frame (`ui_toast`, `ui_status`, `ui_invalidate`, `ui_panes`, ...) as the agent sent it.
    PluginUi {
        event: serde_json::Value,
    },
    TextDelta {
        text: String,
    },
    /// The mode the CLI actually started in, which can differ from the one asked for.
    PermissionModeReported {
        mode: String,
    },
    #[serde(rename_all = "camelCase")]
    GeneratedImage {
        id: String,
        path: String,
        name: String,
        mime_type: String,
    },
    ReasoningDelta {
        text: String,
    },
    #[serde(rename_all = "camelCase")]
    AssistantMessageCompleted {
        assistant_message_id: String,
    },
    ToolCall {
        id: String,
        call: ToolCall,
    },
    #[serde(rename_all = "camelCase")]
    ToolResult {
        id: String,
        is_error: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        diff: Option<ToolDiff>,
    },
    /// Missing fields keep the previous measurement; zero tokens is valid.
    #[serde(rename_all = "camelCase")]
    ContextUsage {
        tokens: Option<u64>,
        window: Option<u64>,
    },
    Compacting {
        active: bool,
    },
    /// A main-thread assistant message just went out; the prompt cache it touched lives for an hour when `one_hour`, else five minutes.
    #[serde(rename_all = "camelCase")]
    PromptCache {
        one_hour: bool,
    },
    /// The agent's guess at what the user sends next; an empty string withdraws it.
    PromptSuggestion {
        suggestion: String,
    },
    #[serde(rename_all = "camelCase")]
    TaskStarted {
        task_id: String,
        tool_use_id: Option<String>,
        kind: BackgroundTaskKind,
        description: String,
    },
    #[serde(rename_all = "camelCase")]
    TaskProgress {
        task_id: String,
        total_tokens: Option<u64>,
        tool_uses: Option<u64>,
        duration_ms: Option<u64>,
        last_tool: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    TaskFinished {
        task_id: String,
        status: DoneStatus,
    },
    /// The complete set of live background tasks; replaces any earlier set.
    BackgroundTasksChanged {
        tasks: Vec<BackgroundTaskInfo>,
    },
    Compacted {
        tokens: Option<u64>,
        manual: bool,
    },
    #[serde(rename_all = "camelCase")]
    Usage {
        input_tokens: u64,
        output_tokens: u64,
    },
    #[serde(rename_all = "camelCase")]
    AvailableCommands {
        commands: Vec<SlashCommand>,
    },
    /// Whether this account can use Remote Control, as the CLI reports it when the run starts.
    RemoteControlAvailable {
        available: bool,
    },
    Error {
        message: String,
    },
    #[serde(rename_all = "camelCase")]
    InputRequested {
        request_id: String,
        questions: Vec<UserInputQuestion>,
    },
    #[serde(rename_all = "camelCase")]
    InputResolved {
        request_id: String,
    },
    #[serde(rename_all = "camelCase")]
    Steered {
        assistant_message_id: Option<String>,
        next_assistant_message_id: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    Done {
        status: DoneStatus,
        result: Option<String>,
        error: Option<String>,
        session_id: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    UserMessage {
        text: String,
    },
    /// The agent saved a prompt the chat sent, under this id of its own.
    PromptRecorded {
        id: String,
    },
    TitleChanged {
        title: String,
    },
    #[serde(rename_all = "camelCase")]
    Subagent {
        parent_tool_use_id: String,
        event: Box<AgentEvent>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_is_claude_only() {
        for harness in [
            HarnessId::Codex,
            HarnessId::Opencode,
            HarnessId::Antigravity,
        ] {
            assert!(!PermissionMode::choices(harness).contains(&PermissionMode::Plan));
            assert_eq!(
                PermissionMode::Plan.for_harness(harness),
                PermissionMode::Auto
            );
        }
        assert!(PermissionMode::choices(HarnessId::ClaudeCode).contains(&PermissionMode::Plan));
        assert_eq!(
            PermissionMode::Plan.for_harness(HarnessId::ClaudeCode),
            PermissionMode::Plan
        );
    }

    #[test]
    fn agent_event_round_trips() {
        let ev = AgentEvent::ToolCall {
            id: "t1".into(),
            call: ToolCall::Exec {
                command: "cargo test".into(),
            },
        };
        let json = serde_json::to_string(&ev).unwrap();
        assert_eq!(serde_json::from_str::<AgentEvent>(&json).unwrap(), ev);
    }

    #[test]
    fn subagent_model_reads_every_spelling_and_only_off_a_spawn() {
        let spawn = |input: serde_json::Value| ToolCall::Unknown {
            name: "Agent: scan".into(),
            input: Some(input),
        };
        for key in SUBAGENT_MODEL_KEYS {
            let call = spawn(serde_json::json!({ key: "haiku" }));
            assert_eq!(call.subagent_model(), Some("haiku"), "key {key}");
        }
        assert_eq!(
            ToolCall::Mcp {
                server: "s".into(),
                tool: "Agent: scan".into(),
                input: Some(serde_json::json!({ "model": "sonnet" })),
            }
            .subagent_model(),
            Some("sonnet")
        );
        assert_eq!(
            ToolCall::Unknown {
                name: "Bash".into(),
                input: Some(serde_json::json!({ "model": "haiku" })),
            }
            .subagent_model(),
            None
        );
        assert_eq!(
            spawn(serde_json::json!({ "model": " " })).subagent_model(),
            None
        );
        assert_eq!(
            spawn(serde_json::json!({ "prompt": "x" })).subagent_model(),
            None
        );
        assert_eq!(
            ToolCall::Unknown {
                name: "Agent".into(),
                input: None
            }
            .subagent_model(),
            None
        );
        assert_eq!(
            spawn(serde_json::json!({ "model": 5 })).subagent_model(),
            None
        );
    }
}

#[cfg(test)]
mod generated_image_tests {
    use super::*;

    #[test]
    fn generated_image_event_round_trip() {
        let event = AgentEvent::GeneratedImage {
            id: "item:image".into(),
            path: "/uploads/generated.png".into(),
            name: "generated.png".into(),
            mime_type: "image/png".into(),
        };
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["type"], "generatedImage");
        assert_eq!(value["mimeType"], "image/png");
        assert_eq!(serde_json::from_value::<AgentEvent>(value).unwrap(), event);
    }
}

#[cfg(test)]
mod todo_item_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn legacy_items_without_status_derive_from_done() {
        let done: TodoItem = serde_json::from_value(json!({ "text": "a", "done": true })).unwrap();
        let open: TodoItem = serde_json::from_value(json!({ "text": "b", "done": false })).unwrap();
        assert_eq!(done.status(), TodoStatus::Completed);
        assert_eq!(open.status(), TodoStatus::Pending);
        assert_eq!(open.status, None);
    }

    #[test]
    fn only_in_progress_adds_a_field_so_other_docs_are_unchanged() {
        let pending = serde_json::to_value(TodoItem::new("a", TodoStatus::Pending)).unwrap();
        let done = serde_json::to_value(TodoItem::new("b", TodoStatus::Completed)).unwrap();
        let active = serde_json::to_value(TodoItem::new("c", TodoStatus::InProgress)).unwrap();
        assert_eq!(pending, json!({ "text": "a", "done": false }));
        assert_eq!(done, json!({ "text": "b", "done": true }));
        assert_eq!(
            active,
            json!({ "text": "c", "done": false, "status": "inProgress" })
        );
    }

    #[test]
    fn unknown_status_falls_back_to_done_instead_of_failing_the_call() {
        let call: ToolCall = serde_json::from_value(json!({
            "kind": "todo",
            "items": [
                { "text": "a", "done": false, "status": "blocked" },
                { "text": "b", "done": true, "status": 7 },
            ]
        }))
        .unwrap();
        let ToolCall::Todo { items } = call else {
            panic!("expected a todo call");
        };
        assert_eq!(items[0].status(), TodoStatus::Pending);
        assert_eq!(items[1].status(), TodoStatus::Completed);
    }

    #[test]
    fn done_outranks_a_stale_status() {
        let item = TodoItem {
            text: "a".into(),
            done: true,
            status: Some(TodoStatus::InProgress),
        };
        assert_eq!(item.status(), TodoStatus::Completed);
    }

    #[test]
    fn harness_status_strings_decode() {
        assert_eq!(TodoStatus::parse("completed"), TodoStatus::Completed);
        assert_eq!(TodoStatus::parse("in_progress"), TodoStatus::InProgress);
        assert_eq!(TodoStatus::parse("inProgress"), TodoStatus::InProgress);
        assert_eq!(TodoStatus::parse("pending"), TodoStatus::Pending);
        assert_eq!(TodoStatus::parse("cancelled"), TodoStatus::Pending);
        assert_eq!(TodoStatus::parse(""), TodoStatus::Pending);
    }
}
