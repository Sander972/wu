pub mod catalog;
mod discovery;
mod history;
mod normalize;
mod wire;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;
use i18n::{n, t};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;

use crate::{
    AgentEvent, DoneStatus, ExternalSession, HarnessId, Model, PermissionDecision, PermissionMode,
    PermissionPrompt, ReasoningLevel, RunRequest, SkillRef, SlashCommand, SteeringMode,
    UserInputAnswer, UserInputQuestion,
};

use crate::process::{Child, ChildStdin, Command, Stdio};
use crate::{
    BeforeFileToolFn, Harness, HarnessError, McpMessageFn, RunControls, Steer, TurnSettings,
    shutdown_child,
};
use catalog::{apply_ultrathink, to_effort};
use normalize::Normalizer;
use wire::{
    ControlRequestBody, ControlRequestFrame, Frame, allow_response, control_error_line,
    control_response_line, permission_response,
};

pub const PERMISSION_ALLOW: &str = "Allow";
pub const PERMISSION_DENY: &str = "Deny";
pub const PERMISSION_HEADER: &str = "Permission";
pub const QUESTION_HEADER: &str = n!("Question");
/// The notice for a turn Claude refused because it isn't signed in; kept untranslated so the chat can tell it apart and offer to sign in.
pub const SIGN_IN_REQUIRED: &str = n!("Authentication failed. Sign in to Claude again.");

pub fn asks_to_sign_in(notice: &str) -> bool {
    notice == SIGN_IN_REQUIRED
}
pub const DENY_STOP: &str = "The user doesn't want to proceed with this tool use. The tool use was rejected (eg. if it was a file edit, the new_string was NOT written to the file). STOP what you are doing and wait for the user to tell you how to proceed.";
pub const DENY_REASON_PREFIX: &str = "The user doesn't want to proceed with this tool use. The tool use was rejected (eg. if it was a file edit, the new_string was NOT written to the file). The user provided the following reason for the rejection: ";
pub const KEEP_PLANNING: &str = "User chose to stay in plan mode and continue planning";

fn permission_question(request: &ControlRequestBody) -> UserInputQuestion {
    let tool_name = request.tool_name.as_str();
    let call = normalize::decode_tool_use(tool_name, &request.input);
    let question = match &call {
        _ if tool_name == "ExitPlanMode" => t!("Ready to code?"),
        crate::ToolCall::Exec { command } => t!("Run `{command}`?", command = command),
        crate::ToolCall::WriteFile { path, .. } => t!("Write {path}?", path = path),
        crate::ToolCall::EditFile { path, .. } => t!("Edit {path}?", path = path),
        crate::ToolCall::ReadFile { path } => t!("Read {path}?", path = path),
        crate::ToolCall::WebFetch { url, .. } => t!("Fetch {url}?", url = url),
        crate::ToolCall::WebSearch { query } => {
            t!("Search the web for \"{query}\"?", query = query)
        }
        crate::ToolCall::Mcp { server, tool, .. } => {
            t!("Use {server}: {tool}?", server = server, tool = tool)
        }
        _ => t!("Use {tool}?", tool = tool_name),
    }
    .to_string();
    let is_set = |key: &str| {
        !matches!(
            request.other.get(key),
            None | Some(Value::Null) | Some(Value::Bool(false))
        )
    };
    let suggestions = request
        .other
        .get("permission_suggestions")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let allows_always =
        !suggestions.is_empty() && !is_set("suppress_always_allow_rule") && !is_set("plugin");
    UserInputQuestion {
        id: uuid::Uuid::new_v4().to_string(),
        header: PERMISSION_HEADER.into(),
        question,
        options: vec![PERMISSION_ALLOW.into(), PERMISSION_DENY.into()],
        multi_select: false,
        prefill: None,
        multiline: false,
        accepts_typed_answer: false,
        permission: Some(PermissionPrompt {
            tool_name: tool_name.into(),
            input: request.input.clone(),
            tool_use_id: request.text("tool_use_id").map(Into::into),
            suggestions,
            allows_always,
            blocked_path: request.text("blocked_path").map(Into::into),
            title: request.text("title").map(Into::into),
            description: request.text("description").map(Into::into),
        }),
    }
}

fn resolve_claude_executable() -> Option<PathBuf> {
    let mut extra = Vec::new();
    if let Some(home) = crate::executable::home_dir() {
        extra.push(home.join(".claude").join("local").join("claude"));
        extra.push(home.join(".local").join("bin").join("claude"));
    }
    extra.push(PathBuf::from("/opt/homebrew/bin/claude"));
    extra.push(PathBuf::from("/usr/local/bin/claude"));
    crate::executable::find_on_paths("claude", extra)
}

/// Thinking is on unless the option says otherwise, like the CLI's own default.
fn thinking_enabled(options: &serde_json::Map<String, Value>) -> bool {
    match options.get(catalog::THINKING_OPTION) {
        Some(Value::Bool(on)) => *on,
        Some(Value::String(choice)) => choice != "off" && choice != "false",
        _ => true,
    }
}

fn option_is_on(options: &serde_json::Map<String, Value>, key: &str) -> bool {
    match options.get(key) {
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => s == "on" || s == "true",
        _ => false,
    }
}

pub struct ClaudeHarness {
    executable: Option<PathBuf>,
    config_dir: Option<PathBuf>,
    titles: Arc<history::TitleCache>,
    interrupt_grace: Duration,
    kill_grace: Duration,
    initialize: discovery::InitializeCache,
    models_cache: crate::catalog::Catalog,
}

impl Default for ClaudeHarness {
    fn default() -> Self {
        Self {
            executable: None,
            config_dir: None,
            titles: Arc::default(),
            interrupt_grace: Duration::from_secs(2),
            kill_grace: Duration::from_secs(3),
            initialize: discovery::InitializeCache::default(),
            models_cache: crate::catalog::Catalog::default(),
        }
    }
}

impl ClaudeHarness {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_executable(mut self, path: impl Into<PathBuf>) -> Self {
        self.executable = Some(path.into());
        self
    }

    pub fn with_config_dir(mut self, path: impl Into<PathBuf>) -> Self {
        self.config_dir = Some(path.into());
        self
    }

    fn config_dir(&self) -> PathBuf {
        self.config_dir.clone().unwrap_or_else(claude_config_dir)
    }

    pub fn with_graces(mut self, interrupt_grace: Duration, kill_grace: Duration) -> Self {
        self.interrupt_grace = interrupt_grace;
        self.kill_grace = kill_grace;
        self
    }

    fn resolve_executable(&self) -> Result<PathBuf, HarnessError> {
        if let Some(p) = &self.executable {
            return crate::executable::validate_native_override(p);
        }
        if let Some(p) = std::env::var_os("CLAUDE_CODE_EXECUTABLE")
            && !p.is_empty()
        {
            return crate::executable::validate_native_override(&PathBuf::from(p));
        }
        resolve_claude_executable().ok_or_else(|| {
            HarnessError::agent_missing(
                HarnessId::ClaudeCode,
                "searched PATH, ~/.claude/local, \
                 ~/.local/bin, /opt/homebrew/bin, /usr/local/bin, and \
                 fnm/nvm/volta/pnpm/bun install dirs; Windows also checks USERPROFILE \
                 and explicit NVM_SYMLINK/VOLTA_HOME/PNPM_HOME; set \
                 CLAUDE_CODE_EXECUTABLE to override",
            )
        })
    }

    fn build_command(
        &self,
        exe: &PathBuf,
        request: &RunRequest,
        discovered: Option<&[crate::Model]>,
    ) -> Command {
        let mut cmd = crate::process::command(exe);
        crate::compose_child_path(&mut cmd, exe);
        cmd.args([
            "--print",
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
            // The CLI rejects stream-json output without --verbose.
            "--verbose",
            "--include-partial-messages",
            "--replay-user-messages",
            // Undocumented: routes `can_use_tool` requests to the stdio control channel.
            "--permission-prompt-tool",
            "stdio",
        ]);
        if thinking_enabled(&request.model_options) {
            // Newer models emit no thinking text unless a summary is requested.
            cmd.args(["--thinking-display", "summarized"]);
        } else {
            cmd.arg("--thinking=disabled");
        }
        if let Some(model) = &request.model {
            cmd.arg("--model");
            cmd.arg(model_argument_for(
                model,
                &request.model_options,
                discovered,
            ));
        }
        if let Some(effort) = to_effort(request.reasoning, request.model.as_deref()) {
            cmd.args(["--effort", effort]);
        }
        cmd.args(permission_args(request.permission));
        // Lets `rewind_files` restore files to how they were before a given prompt.
        cmd.env("CLAUDE_CODE_ENABLE_SDK_FILE_CHECKPOINTING", "true");
        // A wu started from an IDE's terminal inherits that IDE's port, and a chat must not connect to it.
        cmd.env_remove("CLAUDE_CODE_SSE_PORT");
        // Headless sessions only connect the Chrome extension when asked to.
        cmd.arg(if request.chrome {
            "--chrome"
        } else {
            "--no-chrome"
        });
        if let Some(resume) = &request.resume {
            cmd.arg(format!("--resume={resume}"));
        } else if let Some(fork) = &request.fork {
            cmd.arg(format!("--resume={fork}"));
            cmd.arg("--fork-session");
        }
        if request.ephemeral {
            cmd.arg("--no-session-persistence");
        }
        let settings = flag_settings(request);
        if !settings.is_empty() {
            cmd.arg("--settings");
            cmd.arg(Value::Object(settings).to_string());
        }
        if request.cwd.is_empty() {
            cmd.current_dir(crate::executable::scratch_dir());
        } else {
            cmd.current_dir(&request.cwd);
        }
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        cmd
    }

    fn discovery_context(&self) -> Result<crate::ModelContext, HarnessError> {
        crate::model_context::context(self.id(), &self.resolve_executable()?, &[])
    }

    async fn initialize(&self) -> Result<Value, HarnessError> {
        self.initialize
            .get(
                || self.discovery_context().map(|context| context.key()),
                || self.probe_initialize(None),
            )
            .await
    }

    async fn probe_initialize(&self, cwd: Option<&std::path::Path>) -> Result<Value, HarnessError> {
        let exe = self.resolve_executable()?;
        let mut cmd = crate::process::command(&exe);
        cmd.current_dir(cwd.map_or_else(crate::executable::scratch_dir, std::path::Path::to_path_buf));
        crate::compose_child_path(&mut cmd, &exe);
        cmd.args([
            "--print",
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
            // The CLI rejects stream-json output without --verbose.
            "--verbose",
        ]);
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        let mut child = cmd.spawn().map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                HarnessError::NotInstalled(crate::executable::binary_hint(&exe))
            } else {
                HarnessError::Io(e)
            }
        })?;
        let (Some(mut stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            shutdown_child(&mut child, self.kill_grace).await;
            return Err(HarnessError::Protocol("claude child has no stdio".into()));
        };
        const PROBE_ID: &str = "wu-initialize-probe";
        let discovery = async {
            let request = serde_json::json!({
                "type": "control_request",
                "request_id": PROBE_ID,
                "request": { "subtype": "initialize" },
            });
            stdin
                .write_all(format!("{request}\n").as_bytes())
                .await
                .map_err(HarnessError::Io)?;
            stdin.flush().await.map_err(HarnessError::Io)?;
            let mut lines = BufReader::new(stdout).lines();
            while let Some(line) = lines.next_line().await.map_err(HarnessError::Io)? {
                let Ok(frame) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                if frame.get("type").and_then(Value::as_str) != Some("control_response") {
                    continue;
                }
                let response = frame.get("response").cloned().unwrap_or(Value::Null);
                if response.get("request_id").and_then(Value::as_str) != Some(PROBE_ID) {
                    continue;
                }
                if response.get("subtype").and_then(Value::as_str) == Some("error") {
                    let msg = response
                        .get("error")
                        .and_then(Value::as_str)
                        .unwrap_or("initialize control request failed");
                    return Err(HarnessError::Protocol(msg.into()));
                }
                return Ok(response);
            }
            Err(HarnessError::Protocol(
                "claude exited before answering the initialize control request".into(),
            ))
        };
        let result = tokio::time::timeout(Duration::from_secs(10), discovery).await;
        let kill_grace = self.kill_grace;
        tokio::spawn(async move { shutdown_child(&mut child, kill_grace).await });
        match result {
            Ok(inner) => inner,
            Err(_) => Err(HarnessError::Protocol(
                n!("Claude initialize timed out").into(),
            )),
        }
    }
}

// Settings and info commands answer outside the chat, and Wu has its own controls for them; every Wu slash command name is here too, so a CLI command never pushes Wu's aside to `/wu:<name>`.
const HIDDEN_COMMANDS: &[&str] = &[
    "__remote-workflow",
    "add-dir",
    "advisor",
    "agents",
    "auto-mode-setup",
    "autocompact",
    "bashes",
    "btw",
    "bug",
    "clear",
    "color",
    "config",
    "context",
    "copy",
    "cost",
    "design",
    "design-consent",
    "design-revoke",
    "diff",
    "effort",
    "exit",
    "export",
    "extra-usage",
    "fast",
    "feedback",
    "files",
    "focus",
    "fork",
    "heapdump",
    "help",
    "hooks",
    "ide",
    "import",
    "install-github-app",
    "keybindings",
    "list-agents",
    "login",
    "logout",
    "mcp",
    "memory",
    "migrate-installer",
    "model",
    "new",
    "output-style",
    "permissions",
    "plan",
    "plugin",
    "plugins",
    "privacy-settings",
    "quit",
    "recap",
    "release-notes",
    "reload-plugins",
    "reload-skills",
    "rename",
    "resume",
    "rewind",
    "sandbox",
    "settings",
    "skill-doctor",
    "skills",
    "stats",
    "status",
    "statusline",
    "stickers",
    "stop",
    "tasks",
    "terminal",
    "terminal-setup",
    "theme",
    "todos",
    "upgrade",
    "usage",
    "usage-credits",
    "vim",
    "workflow-launch-exec",
];

pub fn is_hidden_command(name: &str) -> bool {
    HIDDEN_COMMANDS.contains(&name)
}

pub fn claude_config_dir() -> PathBuf {
    std::env::var_os("CLAUDE_CONFIG_DIR")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| crate::executable::home_or_current_dir().join(".claude"))
}

fn claude_roots(
    cwd: &std::path::Path,
    config_dir: &std::path::Path,
) -> impl Iterator<Item = PathBuf> {
    cwd.ancestors()
        .map(|dir| dir.join(".claude"))
        .chain(std::iter::once(config_dir.to_path_buf()))
}

fn defined_command_names(cwd: &std::path::Path, config_dir: &std::path::Path) -> HashSet<String> {
    let mut names = HashSet::new();
    for root in claude_roots(cwd, config_dir) {
        collect_markdown_stems(&root.join("commands"), &mut names);
        collect_skill_names(&root.join("skills"), &mut names);
    }
    names
}

fn defined_skill_names(cwd: &std::path::Path, config_dir: &std::path::Path) -> HashSet<String> {
    let mut names = HashSet::new();
    for root in claude_roots(cwd, config_dir) {
        collect_skill_names(&root.join("skills"), &mut names);
    }
    collect_plugin_skill_names(&config_dir.join("plugins"), 0, &mut names);
    names
}

fn collect_skill_names(dir: &std::path::Path, names: &mut HashSet<String>) {
    let Ok(skills) = std::fs::read_dir(dir) else {
        return;
    };
    for skill in skills.flatten() {
        if skill.path().join("SKILL.md").is_file() {
            names.insert(skill.file_name().to_string_lossy().into_owned());
        }
    }
}

const MAX_PLUGIN_DEPTH: usize = 7;

/// Plugin skills are named `plugin:skill`; the plugin folder is one or two levels above `skills`.
fn collect_plugin_skill_names(dir: &std::path::Path, depth: usize, names: &mut HashSet<String>) {
    if depth > MAX_PLUGIN_DEPTH {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        let path = entry.path();
        if entry.file_name() == "skills" {
            let mut skills = HashSet::new();
            collect_skill_names(&path, &mut skills);
            let plugins = path
                .ancestors()
                .skip(1)
                .take(2)
                .filter_map(|folder| folder.file_name())
                .map(|folder| folder.to_string_lossy().into_owned())
                .collect::<Vec<_>>();
            for skill in &skills {
                for plugin in &plugins {
                    names.insert(format!("{plugin}:{skill}"));
                }
            }
        } else {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if !name.starts_with('.') && name != "node_modules" {
                collect_plugin_skill_names(&path, depth + 1, names);
            }
        }
    }
}

fn collect_markdown_stems(dir: &std::path::Path, names: &mut HashSet<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            collect_markdown_stems(&path, names);
        } else if path.extension().is_some_and(|extension| extension == "md")
            && let Some(stem) = path.file_stem()
        {
            names.insert(stem.to_string_lossy().into_owned());
        }
    }
}

fn with_skills(prompt: &str, skills: &[SkillRef]) -> String {
    let mut names: Vec<&str> = Vec::new();
    for skill in skills {
        if !names.contains(&skill.name.as_str()) {
            names.push(&skill.name);
        }
    }
    if names.is_empty() {
        return prompt.to_owned();
    }
    let list = names
        .iter()
        .map(|name| format!("`{name}`"))
        .collect::<Vec<_>>()
        .join(", ");
    format!("{prompt}\n\nBefore you start, use these skills with the `Skill` tool: {list}.")
}

fn parse_initialize_commands(response: &Value) -> Vec<SlashCommand> {
    response
        .pointer("/response/commands")
        .and_then(Value::as_array)
        .map(|commands| commands.as_slice())
        .unwrap_or_default()
        .iter()
        .filter_map(|command| {
            let name = command.get("name").and_then(Value::as_str)?.trim();
            if name.is_empty() {
                return None;
            }
            Some(SlashCommand {
                name: name.to_owned(),
                description: command
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                input_hint: command
                    .get("argumentHint")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|hint| !hint.is_empty())
                    .map(str::to_owned),
                is_skill: false,
            })
        })
        .collect()
}

async fn listed_commands(
    initialize_response: &Value,
    project: PathBuf,
    config_dir: PathBuf,
) -> Result<Vec<SlashCommand>, HarnessError> {
    let mut commands = parse_initialize_commands(initialize_response);
    let (defined, skills) = tokio::task::spawn_blocking(move || {
        (
            defined_command_names(&project, &config_dir),
            defined_skill_names(&project, &config_dir),
        )
    })
    .await
    .map_err(|error| HarnessError::Protocol(error.to_string()))?;
    commands.retain(|command| !is_hidden_command(&command.name) || defined.contains(&command.name));
    for command in &mut commands {
        command.is_skill = skills.contains(&command.name);
    }
    Ok(commands)
}

#[async_trait]
impl Harness for ClaudeHarness {
    fn id(&self) -> HarnessId {
        HarnessId::ClaudeCode
    }
    fn display_name(&self) -> &str {
        "Claude Code"
    }
    fn supports_steering(&self) -> bool {
        true
    }
    fn steering_mode(&self) -> SteeringMode {
        SteeringMode::StepBoundary
    }
    fn reasoning_levels(&self) -> &[ReasoningLevel] {
        &[
            ReasoningLevel::Low,
            ReasoningLevel::Medium,
            ReasoningLevel::High,
            ReasoningLevel::XHigh,
            ReasoningLevel::Max,
        ]
    }
    fn stops_turn_in_place(&self) -> bool {
        true
    }
    fn applies_settings_live(&self, before: &TurnSettings, after: &TurnSettings) -> bool {
        let launch_only = |settings: &TurnSettings| {
            let mut options = settings.model_options.clone();
            options.remove(catalog::CONTEXT_WINDOW_OPTION);
            options.remove(catalog::THINKING_OPTION);
            (
                to_effort(settings.reasoning, settings.model.as_deref()),
                settings.reasoning == Some(ReasoningLevel::Ultracode),
                options,
            )
        };
        after.model.is_some() && launch_only(before) == launch_only(after)
    }
    fn installed(&self) -> bool {
        // Must match launch resolution, including CLAUDE_CODE_EXECUTABLE.
        self.resolve_executable().is_ok()
    }
    fn executable_path(&self) -> Option<PathBuf> {
        self.resolve_executable().ok()
    }
    fn update_args(&self) -> &'static [&'static str] {
        &["update"]
    }
    fn model_context(&self) -> Result<Option<crate::ModelContext>, HarnessError> {
        self.discovery_context().map(Some)
    }
    fn fallback_models(&self) -> Vec<Model> {
        catalog::configured_models()
    }
    async fn model_catalog(&self, force: bool) -> Result<crate::ModelCatalog, HarnessError> {
        self.discovery_context()?.log();
        self.models_cache
            .get_with_timeout(
                force,
                Duration::from_secs(35),
                || self.discovery_context().map(|context| context.key()),
                || async {
                    let response = self.initialize().await?;
                    catalog::with_discovered_models(catalog::configured_models(), &response)
                },
            )
            .await
    }
    async fn commands(&self, cwd: &std::path::Path) -> Result<Vec<SlashCommand>, HarnessError> {
        let response = self.probe_initialize(Some(cwd)).await?;
        listed_commands(&response, cwd.to_owned(), self.config_dir()).await
    }

    async fn external_sessions(
        &self,
        cwd: &std::path::Path,
    ) -> Result<Vec<ExternalSession>, HarnessError> {
        let config_dir = self.config_dir();
        let cwd = cwd.to_owned();
        let titles = self.titles.clone();
        tokio::task::spawn_blocking(move || history::list_sessions(&config_dir, &cwd, &titles))
            .await
            .map_err(|error| HarnessError::Protocol(error.to_string()))
    }

    async fn external_history(
        &self,
        cwd: &std::path::Path,
        session_id: &str,
    ) -> Result<Vec<AgentEvent>, HarnessError> {
        let config_dir = self.config_dir();
        let cwd = cwd.to_owned();
        let session_id = session_id.to_owned();
        tokio::task::spawn_blocking(move || history::read_history(&config_dir, &cwd, &session_id))
            .await
            .map_err(|error| HarnessError::Protocol(error.to_string()))?
    }

    async fn models(&self) -> Result<Vec<Model>, HarnessError> {
        self.resolve_executable()?;
        match self.model_catalog(false).await {
            Ok(catalog) => Ok(catalog.models),
            Err(error) => {
                tracing::warn!(%error, source = "static", "Claude model discovery failed");
                Ok(self.fallback_models())
            }
        }
    }

    async fn control_once(
        &self,
        cwd: &std::path::Path,
        resume: Option<&str>,
        request: Value,
    ) -> Result<Value, HarnessError> {
        self.open_control_session(cwd, resume)
            .await?
            .call(request)
            .await
    }

    async fn run(
        &self,
        request: RunRequest,
        controls: RunControls,
    ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
        self.run_with_mode(request, controls).await
    }
}

type PendingReplies = Arc<
    std::sync::Mutex<Option<HashMap<String, tokio::sync::oneshot::Sender<Result<Value, String>>>>>,
>;

/// One Claude process that answers several control requests, which can be in flight together.
pub struct ControlSession {
    stdin: ChildStdin,
    // `None` once the process stopped answering, so later requests fail instead of waiting forever.
    pending: PendingReplies,
    reader: tokio::task::JoinHandle<()>,
    child: Option<Child>,
    kill_grace: Duration,
}

impl ControlSession {
    pub async fn call(&mut self, request: Value) -> Result<Value, HarnessError> {
        self.send(request).await?.await
    }

    /// Sends `request` and returns its reply to await later, so other requests can go out meanwhile.
    pub async fn send(
        &mut self,
        request: Value,
    ) -> Result<impl Future<Output = Result<Value, HarnessError>> + Send + 'static, HarnessError>
    {
        let exited =
            || HarnessError::Protocol("claude exited before answering the control request".into());
        let request_id = format!("{CONTROL_REQUEST_PREFIX}{}", uuid::Uuid::new_v4());
        let (reply, answer) = tokio::sync::oneshot::channel();
        self.pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_mut()
            .ok_or_else(exited)?
            .insert(request_id.clone(), reply);
        let line = wire::control_request_line(&request_id, &request);
        self.stdin.write_all(line.as_bytes()).await?;
        self.stdin.write_all(b"\n").await?;
        self.stdin.flush().await?;
        Ok(async move {
            answer
                .await
                .map_err(|_| exited())?
                .map_err(HarnessError::Protocol)
        })
    }
}

impl Drop for ControlSession {
    fn drop(&mut self) {
        self.reader.abort();
        // Without a runtime the child still dies: it is spawned with kill_on_drop.
        if let Some(mut child) = self.child.take()
            && let Ok(runtime) = tokio::runtime::Handle::try_current()
        {
            let kill_grace = self.kill_grace;
            runtime.spawn(async move { shutdown_child(&mut child, kill_grace).await });
        }
    }
}

/// Where to sign in: `automatic` returns to the CLI by itself, `manual` shows a code to paste.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoginUrls {
    pub automatic: Option<String>,
    pub manual: Option<String>,
}

fn web_url(response: &Value, key: &str) -> Option<String> {
    response
        .get(key)
        .and_then(Value::as_str)
        .filter(|url| url.starts_with("https://"))
        .map(str::to_owned)
}

const LOGIN_START_TIMEOUT: Duration = Duration::from_secs(60);

/// Splits a pasted `code#state`; a bare code takes the state of the sign-in address, or none.
fn pasted_code(pasted: &str, authenticate: &Value) -> (String, String) {
    let pasted = pasted.trim();
    match pasted.split_once('#') {
        Some((code, state)) => (code.to_owned(), state.to_owned()),
        None => (
            pasted.to_owned(),
            authenticate
                .get("manualUrl")
                .and_then(Value::as_str)
                .and_then(|url| url.split_once('?'))
                .and_then(|(_, query)| {
                    query
                        .split('&')
                        .find_map(|pair| pair.strip_prefix("state="))
                })
                .unwrap_or_default()
                .to_owned(),
        ),
    }
}

impl ClaudeHarness {
    /// Starts a throwaway Claude process in `cwd` for control requests that must reach the same process.
    pub async fn control_session(
        &self,
        cwd: &std::path::Path,
    ) -> Result<ControlSession, HarnessError> {
        self.open_control_session(cwd, None).await
    }

    async fn open_control_session(
        &self,
        cwd: &std::path::Path,
        resume: Option<&str>,
    ) -> Result<ControlSession, HarnessError> {
        let exe = self.resolve_executable()?;
        let launch = RunRequest {
            prompt: String::new(),
            model: None,
            reasoning: None,
            model_options: serde_json::Map::new(),
            cwd: cwd.to_string_lossy().into_owned(),
            permission: PermissionMode::default(),
            resume: resume.map(str::to_owned),
            fork: None,
            attachments: Vec::new(),
            skills: Vec::new(),
            auto_compact_tokens: None,
            chrome: false,
            drop_prompt: None,
            ephemeral: resume.is_none(),
            context: Vec::new(),
        };
        let mut cmd = self.build_command(&exe, &launch, None);
        cmd.stderr(Stdio::null());
        let mut child = cmd.spawn().map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                HarnessError::NotInstalled(crate::executable::binary_hint(&exe))
            } else {
                HarnessError::Io(e)
            }
        })?;
        let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            shutdown_child(&mut child, self.kill_grace).await;
            return Err(HarnessError::Protocol("claude child has no stdio".into()));
        };
        let pending: PendingReplies = Arc::new(std::sync::Mutex::new(Some(HashMap::new())));
        let reader = tokio::spawn({
            let pending = pending.clone();
            async move {
                let mut lines = BufReader::new(stdout).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    let Some((request_id, answer)) =
                        wire::control_reply(&line, CONTROL_REQUEST_PREFIX)
                    else {
                        continue;
                    };
                    let reply = pending
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .as_mut()
                        .and_then(|pending| pending.remove(&request_id));
                    if let Some(reply) = reply {
                        reply.send(answer).ok();
                    }
                }
                pending
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .take();
            }
        });
        let mut session = ControlSession {
            stdin,
            pending,
            reader,
            child: Some(child),
            kill_grace: self.kill_grace,
        };
        session
            .call(serde_json::json!({ "subtype": "initialize" }))
            .await?;
        Ok(session)
    }

    /// Signs in to Claude in one process: `urls` gets the sign-in addresses, then either the browser returns to the CLI by itself or a code pasted into `codes` finishes the sign-in.
    pub async fn login(
        &self,
        cwd: &std::path::Path,
        urls: tokio::sync::oneshot::Sender<LoginUrls>,
        mut codes: mpsc::Receiver<String>,
    ) -> Result<Value, HarnessError> {
        let (mut session, authenticate) = tokio::time::timeout(LOGIN_START_TIMEOUT, async {
            let mut session = self.control_session(cwd).await?;
            let authenticate = session
                .call(serde_json::json!({
                    "subtype": "claude_authenticate",
                    "loginWithClaudeAi": true,
                }))
                .await?;
            Ok::<_, HarnessError>((session, authenticate))
        })
        .await
        .map_err(|_| {
            HarnessError::Protocol(
                t!("Claude Code didn't start the sign-in within a minute. Try again.").to_string(),
            )
        })??;
        urls.send(LoginUrls {
            automatic: web_url(&authenticate, "automaticUrl"),
            manual: web_url(&authenticate, "manualUrl"),
        })
        .ok();
        // Sent before any code: the CLI forgets the flow once it completes, and the browser can complete it without one.
        let completion = session
            .send(serde_json::json!({ "subtype": "claude_oauth_wait_for_completion" }))
            .await?;
        tokio::pin!(completion);
        tokio::select! {
            completed = &mut completion => completed,
            Some(pasted) = codes.recv() => {
                let (code, state) = pasted_code(&pasted, &authenticate);
                let callback = session
                    .send(serde_json::json!({
                        "subtype": "claude_oauth_callback",
                        "authorizationCode": code,
                        "state": state,
                    }))
                    .await?;
                // The callback answers when the sign-in ends, with the clearer error when the code is refused.
                callback.await?;
                completion.await
            }
        }
    }

    async fn run_with_mode(
        &self,
        mut request: RunRequest,
        controls: RunControls,
    ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
        let exe = self.resolve_executable()?;
        let mut resume_at = None;
        let mut drop_problem = None;
        if let (Some(session_id), Some(prompt)) = (
            request.resume.clone().or_else(|| request.fork.clone()),
            request.drop_prompt.take(),
        ) {
            let config_dir = self.config_dir();
            let point = tokio::task::spawn_blocking(move || {
                history::resume_point(&config_dir, &session_id, &prompt)
            })
            .await
            .unwrap_or(history::ResumePoint::NotFound);
            match point {
                history::ResumePoint::At(id) => resume_at = Some(id),
                history::ResumePoint::Empty => {
                    request.resume = None;
                    request.fork = None;
                }
                history::ResumePoint::NotFound => drop_problem = Some(
                    t!("Claude Code couldn't find your earlier message, so it may still see it.")
                        .to_string(),
                ),
            }
        }
        let discovered = self.models_cache.peek();
        let mut cmd = self.build_command(&exe, &request, discovered.as_deref());
        if let Some(id) = &resume_at {
            cmd.arg(format!("--resume-session-at={id}"));
        }
        let normalizer = if let Some(session_id) = request.resume.as_ref().or(request.fork.as_ref())
        {
            Normalizer::for_resume(&self.config_dir(), session_id).await
        } else {
            Normalizer::new()
        };
        let mut child = cmd.spawn().map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                HarnessError::NotInstalled(crate::executable::binary_hint(&exe))
            } else {
                HarnessError::Io(e)
            }
        })?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| HarnessError::Protocol("claude child has no stdin".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| HarnessError::Protocol("claude child has no stdout".into()))?;
        let stderr_tail = crate::StderrTail::default();
        if let Some(stderr) = child.stderr.take() {
            let tail = stderr_tail.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    log::debug!("stderr: {line}");
                    tail.push(&line);
                }
            });
        }

        let (stdin_tx, stdin_rx) = mpsc::unbounded_channel::<StdinMsg>();
        tokio::spawn(stdin_writer(stdin, stdin_rx));

        let commands_scan =
            (!request.cwd.is_empty()).then(|| (PathBuf::from(&request.cwd), self.config_dir()));
        if commands_scan.is_some() {
            let line = wire::initialize_request_line(RUN_INITIALIZE_REQUEST, true);
            stdin_tx.send(StdinMsg::Line(line)).ok();
        }

        let (images, image_problems) = load_attachment_blocks(&request.attachments).await;
        let first_prompt_id = uuid::Uuid::new_v4().to_string();
        let first = wire::user_message_line(
            &apply_ultrathink(
                request.reasoning,
                &with_skills(&request.prompt, &request.skills),
            ),
            &images,
            &request.context,
            &first_prompt_id,
        );
        stdin_tx.send(StdinMsg::Line(first)).ok();

        let (event_tx, event_rx) = mpsc::channel::<Result<AgentEvent, HarnessError>>(256);
        for message in drop_problem.into_iter().chain(image_problems) {
            if event_tx.try_send(Ok(AgentEvent::Error { message })).is_err() {
                log::debug!("claude run consumer gone before an image problem was reported");
            }
        }
        tokio::spawn(run_session(Session {
            normalizer,
            first_prompt_id,
            skip_prompts: request.permission.skips_prompts(),
            child,
            stdout_lines: BufReader::new(stdout).lines(),
            stdin_tx,
            event_tx,
            controls,
            reasoning: request.reasoning,
            model_argument: request.model.as_deref().map(|model| {
                model_argument_for(model, &request.model_options, discovered.as_deref())
            }),
            thinking_on: thinking_enabled(&request.model_options),
            discovered,
            commands_scan,
            interrupt_grace: self.interrupt_grace,
            kill_grace: self.kill_grace,
            stderr_tail,
        }));

        Ok(futures::stream::unfold(event_rx, |mut rx| async move {
            rx.recv().await.map(|ev| (ev, rx))
        })
        .boxed())
    }
}

enum StdinMsg {
    Line(String),
    Close,
}

// The API rejects larger inline images.
const MAX_INLINE_IMAGE_BYTES: usize = 5 * 1024 * 1024;
const MAX_PDF_BYTES: u64 = 32 * 1024 * 1024;

pub(crate) fn image_media_type(path: &std::path::Path, bytes: &[u8]) -> Option<&'static str> {
    let by_extension = match path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("png") => Some("image/png"),
        Some("jpg" | "jpeg") => Some("image/jpeg"),
        Some("gif") => Some("image/gif"),
        Some("webp") => Some("image/webp"),
        _ => None,
    };
    let by_content = match bytes {
        [0x89, b'P', b'N', b'G', ..] => Some("image/png"),
        [0xFF, 0xD8, 0xFF, ..] => Some("image/jpeg"),
        [b'G', b'I', b'F', b'8', ..] => Some("image/gif"),
        [b'R', b'I', b'F', b'F', _, _, _, _, rest @ ..] if rest.starts_with(b"WEBP") => {
            Some("image/webp")
        }
        _ => None,
    };
    by_content.or(by_extension)
}

async fn load_attachment_blocks(paths: &[String]) -> (Vec<wire::AttachmentBlock>, Vec<String>) {
    use base64::Engine as _;
    let mut blocks = Vec::new();
    let mut problems = Vec::new();
    for path in paths {
        let name = std::path::Path::new(path)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.clone());
        let is_pdf = std::path::Path::new(path)
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("pdf"));
        if is_pdf {
            match tokio::fs::metadata(path).await {
                Ok(metadata) if metadata.len() > MAX_PDF_BYTES => {
                    problems.push(
                        t!(
                            "{name} wasn't sent: Claude only accepts PDFs up to 32 MB.",
                            name = name
                        )
                        .to_string(),
                    );
                    continue;
                }
                Ok(_) => {}
                Err(error) => {
                    problems.push(
                        t!("{name} wasn't sent: {error}", name = name, error = error).to_string(),
                    );
                    continue;
                }
            }
        }
        let bytes = match tokio::fs::read(path).await {
            Ok(bytes) => bytes,
            Err(error) => {
                problems.push(
                    t!("{name} wasn't sent: {error}", name = name, error = error).to_string(),
                );
                continue;
            }
        };
        if is_pdf {
            blocks.push(wire::AttachmentBlock::Pdf {
                title: name,
                base64_data: base64::engine::general_purpose::STANDARD.encode(&bytes),
            });
            continue;
        }
        if bytes.len().div_ceil(3) * 4 > MAX_INLINE_IMAGE_BYTES {
            problems.push(
                t!(
                    "{name} wasn't sent: Claude only accepts images up to 5 MB.",
                    name = name
                )
                .to_string(),
            );
            continue;
        }
        let Some(media_type) = image_media_type(std::path::Path::new(path), &bytes) else {
            problems.push(
                t!(
                    "{name} wasn't sent: only PNG, JPEG, GIF and WebP images are supported.",
                    name = name
                )
                .to_string(),
            );
            continue;
        };
        blocks.push(wire::AttachmentBlock::Image {
            media_type,
            base64_data: base64::engine::general_purpose::STANDARD.encode(&bytes),
        });
    }
    (blocks, problems)
}

async fn stdin_writer(mut stdin: ChildStdin, mut rx: mpsc::UnboundedReceiver<StdinMsg>) {
    while let Some(msg) = rx.recv().await {
        match msg {
            StdinMsg::Line(line) => {
                let write = async {
                    stdin.write_all(line.as_bytes()).await?;
                    stdin.write_all(b"\n").await?;
                    stdin.flush().await
                };
                if let Err(e) = write.await {
                    log::debug!("stdin write failed (tolerated): {e}");
                    return;
                }
            }
            StdinMsg::Close => {
                if let Err(error) = stdin.shutdown().await {
                    log::debug!("claude stdin shutdown failed: {error}");
                }
                return;
            }
        }
    }
}

struct Session {
    normalizer: Normalizer,
    first_prompt_id: String,
    skip_prompts: bool,
    child: Child,
    stdout_lines: tokio::io::Lines<BufReader<crate::process::ChildStdout>>,
    stdin_tx: mpsc::UnboundedSender<StdinMsg>,
    event_tx: mpsc::Sender<Result<AgentEvent, HarnessError>>,
    controls: RunControls,
    reasoning: Option<ReasoningLevel>,
    model_argument: Option<String>,
    thinking_on: bool,
    discovered: Option<Vec<Model>>,
    commands_scan: Option<(PathBuf, PathBuf)>,
    interrupt_grace: Duration,
    kill_grace: Duration,
    stderr_tail: crate::StderrTail,
}

fn model_argument_for(
    model: &str,
    options: &serde_json::Map<String, Value>,
    discovered: Option<&[Model]>,
) -> String {
    let choice = options
        .get(catalog::CONTEXT_WINDOW_OPTION)
        .and_then(Value::as_str);
    catalog::model_argument(model, choice, discovered)
}

fn starts_parent_turn(frame: &Frame) -> bool {
    match frame {
        Frame::System(system) => system.subtype == "init",
        Frame::StreamEvent(event) => event.parent_tool_use_id.is_none(),
        Frame::Assistant(message) => message.parent_tool_use_id.is_none(),
        _ => false,
    }
}

fn report_commands(
    initialize_response: Value,
    project: PathBuf,
    config_dir: PathBuf,
    event_tx: mpsc::Sender<Result<AgentEvent, HarnessError>>,
) {
    if initialize_response.get("subtype").and_then(Value::as_str) == Some("error") {
        log::debug!("claude did not list its commands: {initialize_response}");
        return;
    }
    tokio::spawn(async move {
        match listed_commands(&initialize_response, project, config_dir).await {
            Ok(commands) => {
                if event_tx
                    .send(Ok(AgentEvent::AvailableCommands { commands }))
                    .await
                    .is_err()
                {
                    log::debug!("claude run consumer gone before its commands were reported");
                }
            }
            Err(error) => log::debug!("couldn't list claude commands: {error}"),
        }
    });
}

/// Returns false when the consumer is gone.
async fn release_held_done(
    held_done: &mut Option<(AgentEvent, tokio::time::Instant)>,
    pending_steers: &mut std::collections::VecDeque<String>,
    norm: &mut Normalizer,
    event_tx: &mpsc::Sender<Result<AgentEvent, HarnessError>>,
) -> bool {
    while pending_steers.pop_front().is_some() {
        let (prev, next) = norm.rotate_for_steer();
        let steered = AgentEvent::Steered {
            assistant_message_id: Some(prev),
            next_assistant_message_id: Some(next),
        };
        if event_tx.send(Ok(steered)).await.is_err() {
            return false;
        }
    }
    match held_done.take() {
        Some((done, _)) => event_tx.send(Ok(done)).await.is_ok(),
        None => true,
    }
}

async fn run_session(session: Session) {
    let Session {
        normalizer: mut norm,
        first_prompt_id,
        skip_prompts,
        mut child,
        mut stdout_lines,
        stdin_tx,
        event_tx,
        controls,
        mut reasoning,
        mut model_argument,
        mut thinking_on,
        discovered,
        mut commands_scan,
        interrupt_grace,
        kill_grace,
        stderr_tail,
    } = session;
    let RunControls {
        request_input,
        mut steering,
        interrupt,
        mut permission,
        before_file_tool,
        mcp,
    } = controls;
    let request_input = Arc::new(request_input);
    let launched_bypass = skip_prompts;
    let mut skip_prompts = skip_prompts;
    let mut seen_permission = *permission.borrow();
    let mut permission_open = true;

    let mut pending_steers = std::collections::VecDeque::new();
    let mut unrecorded_prompts = HashSet::from([first_prompt_id]);
    let mut open_tools = HashSet::new();
    let mut steering_open = true;
    let mut interrupted = false;
    let mut interrupt_sent = false;
    let mut any_done = false;
    // Steers can be absorbed without a replay, so a held turn end must time out.
    const HELD_DONE_SETTLE: Duration = Duration::from_secs(5);
    let mut held_done: Option<(AgentEvent, tokio::time::Instant)> = None;
    let mut turn_resumed_after_held_done = false;
    let mut done_after_interrupt = false;
    let mut escalation: Option<tokio::task::JoinHandle<()>> = None;
    let mut turn_running = true;
    let mut stopping_turn = false;
    let mut stop_ack = None;
    let mut stop_kill_starts_at = None;
    let mut model_switched = false;
    let mut request_tasks: Vec<tokio::task::AbortHandle> = Vec::new();
    // Dropped when the loop ends, so callers still waiting get an error instead of hanging.
    let mut pending_controls: HashMap<String, tokio::sync::oneshot::Sender<_>> = HashMap::new();

    'main: loop {
        tokio::select! {
            line = stdout_lines.next_line() => match line {
                Ok(Some(line)) => {
                    let line = line.trim();
                    if line.is_empty() {
                        continue;
                    }
                    if let Some((request_id, answer)) = wire::control_reply(line, CONTROL_REQUEST_PREFIX) {
                        if let Some(reply) = pending_controls.remove(&request_id)
                            && reply.send(answer).is_err()
                        {
                            log::debug!("nothing waited for the claude control reply");
                        }
                        continue;
                    }
                    if let Some((_, deadline)) = held_done.as_mut() {
                        *deadline = tokio::time::Instant::now() + HELD_DONE_SETTLE;
                    }
                    if let Some(error) = wire::control_error(line, SET_MODE_REQUEST_PREFIX) {
                        let message = t!(
                            "Claude Code couldn't switch the permission mode right away, so the new mode applies from your next message. {error}",
                            error = error
                        )
                        .to_string();
                        if event_tx.send(Ok(AgentEvent::Error { message })).await.is_err() { break 'main; }
                        continue;
                    }
                    if let Some(error) = wire::control_error(line, SET_THINKING_REQUEST_PREFIX) {
                        let message = t!("Claude Code couldn't change thinking right away, so it stays as it was. {error}", error = error).to_string();
                        if event_tx.send(Ok(AgentEvent::Error { message })).await.is_err() { break 'main; }
                        continue;
                    }
                    if model_switched
                        && let Some(error) = wire::control_error(line, SET_MODEL_REQUEST_PREFIX)
                    {
                        let message = format!("{} {error}", t!("The selected model isn't available."));
                        if event_tx.send(Ok(AgentEvent::Error { message })).await.is_err() { break 'main; }
                        continue;
                    }
                    if commands_scan.is_some()
                        && let Some(response) = wire::control_response_to(line, RUN_INITIALIZE_REQUEST)
                    {
                        if let Some((project, config_dir)) = commands_scan.take() {
                            report_commands(response, project, config_dir, event_tx.clone());
                        }
                        continue;
                    }
                    let frame = match wire::parse_frame(line) {
                        Ok(frame) => frame,
                        Err(e) => {
                            if let Some(request_id) = wire::unreadable_control_request_id(line) {
                                log::warn!("couldn't read a claude request ({e}); answering with an error");
                                let line = control_error_line(&request_id, "Wu couldn't read this request.");
                                stdin_tx.send(StdinMsg::Line(line)).ok();
                            } else {
                                log::debug!("unparseable frame (skipped): {e}");
                            }
                            continue;
                        }
                    };
                    if let Frame::ControlRequest(req) = frame {
                        // A mode the CLI switched to itself arrives without a change notification, so it isn't sent back.
                        let current = *permission.borrow();
                        if current != seen_permission {
                            seen_permission = current;
                            skip_prompts = current.skips_prompts();
                        }
                        request_tasks.retain(|task| !task.is_finished());
                        request_tasks.extend(handle_control_request(req, skip_prompts, &request_input, &before_file_tool, &mcp, &stdin_tx));
                        continue;
                    }
                    if starts_parent_turn(&frame) {
                        turn_running = true;
                        turn_resumed_after_held_done = held_done.is_some();
                    }
                    if let Frame::User(ref user) = frame {
                        if user.parent_tool_use_id.is_none()
                            && let Some(id) = user.uuid.as_ref().filter(|id| unrecorded_prompts.remove(*id))
                            && event_tx.send(Ok(AgentEvent::PromptRecorded { id: id.clone() })).await.is_err()
                        {
                            break 'main;
                        }
                        // Superseded steers are never replayed, so a replay confirms every earlier steer.
                        if user.parent_tool_use_id.is_none()
                            && let Some(at) = user
                                .uuid
                                .as_ref()
                                .and_then(|id| pending_steers.iter().position(|p| p == id))
                        {
                            for _ in 0..=at {
                                pending_steers.pop_front();
                                let (prev, next) = norm.rotate_for_steer();
                                if event_tx.send(Ok(AgentEvent::Steered {
                                    assistant_message_id: Some(prev), next_assistant_message_id: Some(next),
                                })).await.is_err() { break 'main; }
                            }
                        }
                    }
                    for ev in norm.normalize(frame, interrupted || stopping_turn) {
                        match &ev {
                            AgentEvent::ToolCall { id, .. } => {
                                open_tools.insert(id.clone());
                            }
                            AgentEvent::ToolResult { id, .. } => {
                                open_tools.remove(id);
                            }
                            AgentEvent::Done { .. } => open_tools.clear(),
                            _ => {}
                        }
                        let is_done = matches!(ev, AgentEvent::Done { .. });
                        // A `now` steer ends the turn it interrupts with a result frame.
                        if is_done && !interrupted && !stopping_turn && !pending_steers.is_empty() {
                            held_done =
                                Some((ev, tokio::time::Instant::now() + HELD_DONE_SETTLE));
                            turn_resumed_after_held_done = false;
                            continue;
                        }
                        if is_done {
                            held_done = None;
                        }
                        if is_done
                            && stopping_turn
                            && !interrupted
                            && !crate::confirm_stop(&mut stop_ack, stop_kill_starts_at, &mut escalation)
                        {
                            interrupted = true;
                        }
                        if event_tx.send(Ok(ev)).await.is_err() {
                            break 'main;
                        }
                        if is_done {
                            any_done = true;
                            turn_running = false;
                            if interrupted {
                                done_after_interrupt = true;
                                break 'main;
                            }
                            if std::mem::take(&mut stopping_turn) {
                                if let Some(handle) = escalation.take() {
                                    handle.abort();
                                }
                                pending_steers.clear();
                                unrecorded_prompts.clear();
                                for task in request_tasks.drain(..) {
                                    task.abort();
                                }
                            }
                        }
                    }
                }
                Ok(None) => break 'main,
                Err(e) => {
                    if event_tx.send(Err(HarnessError::Io(e))).await.is_err() {
                        log::debug!("claude run consumer gone before a stdout error was delivered");
                    }
                    break 'main;
                }
            },

            steer = steering.recv(), if steering_open && !interrupted && !stopping_turn => match steer {
                Some(Steer::StopTurn(_)) if !turn_running => {}
                Some(Steer::StopTurn(ack)) if held_done.is_some() && !turn_resumed_after_held_done => {
                    crate::acknowledge_stop(&mut Some(ack));
                    if !release_held_done(&mut held_done, &mut pending_steers, &mut norm, &event_tx).await {
                        break 'main;
                    }
                    any_done = true;
                    turn_running = false;
                    unrecorded_prompts.clear();
                    for task in request_tasks.drain(..) {
                        task.abort();
                    }
                }
                Some(Steer::StopTurn(ack)) => {
                    held_done = None;
                    stopping_turn = true;
                    stop_ack = Some(ack);
                    stop_kill_starts_at = Some(tokio::time::Instant::now() + interrupt_grace);
                    if stdin_tx.send(StdinMsg::Line(wire::interrupt_request_line("stop_1"))).is_err() {
                        log::debug!("claude stdin closed before the turn stop was sent");
                    }
                    escalation = crate::process::escalate_interrupt(&child, interrupt_grace, kill_grace);
                }
                Some(Steer::Message(msg)) => {
                    if let Some(settings) = &msg.settings {
                        reasoning = settings.reasoning;
                        let wanted = settings.model.as_deref().map(|model| {
                            model_argument_for(model, &settings.model_options, discovered.as_deref())
                        });
                        if let Some(wanted) = wanted.filter(|wanted| model_argument.as_ref() != Some(wanted)) {
                            let id = format!("{SET_MODEL_REQUEST_PREFIX}{}", uuid::Uuid::new_v4());
                            if stdin_tx.send(StdinMsg::Line(wire::set_model_line(&id, &wanted))).is_err() { break 'main; }
                            model_argument = Some(wanted);
                            model_switched = true;
                        }
                        let thinking_wanted = thinking_enabled(&settings.model_options);
                        if thinking_wanted != thinking_on {
                            let id = format!("{SET_THINKING_REQUEST_PREFIX}{}", uuid::Uuid::new_v4());
                            if stdin_tx.send(StdinMsg::Line(wire::set_max_thinking_tokens_line(&id, thinking_wanted))).is_err() { break 'main; }
                            thinking_on = thinking_wanted;
                        }
                    }
                    turn_running = true;
                    let id = uuid::Uuid::new_v4().to_string();
                    let (images, image_problems) = load_attachment_blocks(&msg.attachments).await;
                    for message in image_problems {
                        if event_tx.send(Ok(AgentEvent::Error { message })).await.is_err() {
                            break 'main;
                        }
                    }
                    let line = wire::steer_message_line(
                        &apply_ultrathink(reasoning, &with_skills(&msg.prompt, &msg.skills)),
                        &images,
                        &msg.context,
                        &id,
                        open_tools.is_empty(),
                    );
                    unrecorded_prompts.insert(id.clone());
                    pending_steers.push_back(id);
                    if stdin_tx.send(StdinMsg::Line(line)).is_err() { break 'main; }
                }
                Some(Steer::Control(call)) => {
                    let id = format!("{CONTROL_REQUEST_PREFIX}{}", uuid::Uuid::new_v4());
                    if stdin_tx.send(StdinMsg::Line(wire::control_request_line(&id, &call.request))).is_err() { break 'main; }
                    pending_controls.insert(id, call.reply);
                }
                None => {
                    steering_open = false;
                    if stdin_tx.send(StdinMsg::Close).is_err() {
                        log::debug!("claude stdin writer gone before stdin was closed");
                    }
                }
            },

            changed = permission.changed(), if permission_open && !interrupted => match changed {
                Ok(()) => {
                    let mode = *permission.borrow_and_update();
                    seen_permission = mode;
                    skip_prompts = mode.skips_prompts();
                    let id = format!("{SET_MODE_REQUEST_PREFIX}{}", uuid::Uuid::new_v4());
                    let line = wire::set_permission_mode_line(&id, live_permission_mode_name(mode, launched_bypass));
                    if stdin_tx.send(StdinMsg::Line(line)).is_err() { break 'main; }
                }
                Err(_) => permission_open = false,
            },

            _ = interrupt.cancelled(), if !interrupt_sent => {
                interrupt_sent = true;
                interrupted = true;
                if stdin_tx.send(StdinMsg::Line(wire::interrupt_request_line("int_1"))).is_err() {
                    log::debug!("claude stdin closed before the interrupt was sent");
                }
                if escalation.is_none() {
                    escalation = crate::process::escalate_interrupt(&child, interrupt_grace, kill_grace);
                }
            },

            _ = tokio::time::sleep_until(
                held_done.as_ref().map_or_else(tokio::time::Instant::now, |(_, d)| *d)
            ), if held_done.is_some() => {
                if !release_held_done(&mut held_done, &mut pending_steers, &mut norm, &event_tx).await {
                    break 'main;
                }
                any_done = true;
                turn_running = turn_resumed_after_held_done;
            },

            _ = event_tx.closed() => break 'main,
        }
    }

    if let Some((done, _)) = held_done.take()
        && !event_tx.is_closed()
        && event_tx.send(Ok(done)).await.is_ok()
    {
        any_done = true;
    }

    let interrupted = interrupted || stopping_turn;
    if !event_tx.is_closed() {
        if interrupted && !done_after_interrupt {
            if event_tx
                .send(Ok(AgentEvent::Done {
                    status: DoneStatus::Interrupted,
                    result: None,
                    error: None,
                    session_id: norm.session_id.clone(),
                }))
                .await
                .is_err()
            {
                log::debug!("claude run consumer gone before the interrupted Done was delivered");
            }
        } else if !interrupted && !any_done {
            let status = child.try_wait().ok().flatten();
            if event_tx
                .send(Ok(AgentEvent::Done {
                    status: DoneStatus::Errored,
                    result: None,
                    error: Some(crate::crash_message("claude", status, &stderr_tail)),
                    session_id: norm.session_id.clone(),
                }))
                .await
                .is_err()
            {
                log::debug!("claude run consumer gone before the crash Done was delivered");
            }
        }
    }

    shutdown_child(&mut child, kill_grace).await;
    if let Some(handle) = escalation {
        handle.abort();
    }
}

type RequestInputFn = Box<
    dyn Fn(Vec<UserInputQuestion>) -> tokio::sync::oneshot::Receiver<Vec<UserInputAnswer>>
        + Send
        + Sync,
>;

const SET_MODE_REQUEST_PREFIX: &str = "wu-set-mode-";
const SET_MODEL_REQUEST_PREFIX: &str = "wu-set-model-";
const SET_THINKING_REQUEST_PREFIX: &str = "wu-set-thinking-";
const RUN_INITIALIZE_REQUEST: &str = "wu-run-initialize";
const CONTROL_REQUEST_PREFIX: &str = "wu-ctl-";

/// Claude refuses `bypassPermissions` unless launched in it, so it asks instead and Wu approves.
fn live_permission_mode_name(permission: PermissionMode, launched_bypass: bool) -> &'static str {
    match permission_mode_name(permission) {
        "bypassPermissions" if !launched_bypass => "default",
        name => name,
    }
}

/// The CLI names its mode when it starts, so the session can follow it rather than keep the one it asked for.
pub fn permission_mode_from_name(name: &str) -> Option<PermissionMode> {
    PermissionMode::choices(HarnessId::ClaudeCode)
        .iter()
        .copied()
        .find(|mode| permission_mode_name(*mode) == name)
}

pub fn permission_mode_name(permission: PermissionMode) -> &'static str {
    match permission.for_harness(HarnessId::ClaudeCode) {
        PermissionMode::Ask => "default",
        PermissionMode::Plan => "plan",
        PermissionMode::AcceptEdits => "acceptEdits",
        PermissionMode::DontAsk => "dontAsk",
        PermissionMode::FullAccess => "bypassPermissions",
        PermissionMode::Auto | PermissionMode::ReadOnly | PermissionMode::ApproveForMe => "auto",
    }
}

fn permission_args(permission: PermissionMode) -> &'static [&'static str] {
    match permission.for_harness(HarnessId::ClaudeCode) {
        PermissionMode::Ask => &["--permission-mode", "default"],
        PermissionMode::Plan => &["--permission-mode", "plan"],
        PermissionMode::AcceptEdits => &["--permission-mode", "acceptEdits"],
        PermissionMode::DontAsk => &["--permission-mode", "dontAsk"],
        PermissionMode::FullAccess => &[
            "--permission-mode",
            "bypassPermissions",
            "--dangerously-skip-permissions",
        ],
        PermissionMode::Auto | PermissionMode::ReadOnly | PermissionMode::ApproveForMe => {
            &["--permission-mode", "auto"]
        }
    }
}

fn allow_as_asked() -> PermissionDecision {
    PermissionDecision::Allow {
        updated_input: None,
        updated_permissions: Vec::new(),
    }
}

fn deny_and_stop() -> PermissionDecision {
    PermissionDecision::Deny {
        message: DENY_STOP.into(),
        interrupt: true,
    }
}

/// The CLI blocks until every `can_use_tool` request gets a response.
fn handle_control_request(
    req: ControlRequestFrame,
    skip_prompts: bool,
    request_input: &Arc<RequestInputFn>,
    before_file_tool: &BeforeFileToolFn,
    mcp: &McpMessageFn,
    stdin_tx: &mpsc::UnboundedSender<StdinMsg>,
) -> Option<tokio::task::AbortHandle> {
    match req.request.subtype.as_str() {
        "can_use_tool" => {}
        "elicitation" => return answer_elicitation(req, request_input, stdin_tx),
        // The extension (2.1.331) answers its own save hook with the hook output `{continue: true}`.
        "hook_callback" if req.request.text("callback_id") == Some(wire::PRESAVE_CALLBACK_ID) => {
            return answer_presave_hook(req, before_file_tool, stdin_tx);
        }
        "mcp_message" if req.request.text("server_name") == Some(wire::IDE_MCP_SERVER) => {
            return answer_mcp_message(req, mcp, stdin_tx);
        }
        subtype => {
            log::warn!("claude sent an unsupported {subtype} request; answering with an error");
            let error = format!("Wu can't answer {subtype} requests.");
            let line = control_error_line(&req.request_id, &error);
            stdin_tx.send(StdinMsg::Line(line)).ok();
            return None;
        }
    }
    if req.request.tool_name != "AskUserQuestion" {
        if skip_prompts {
            let tool_use_id = req.request.text("tool_use_id");
            let response = permission_response(&allow_as_asked(), &req.request.input, tool_use_id);
            let line = control_response_line(&req.request_id, response);
            stdin_tx.send(StdinMsg::Line(line)).ok();
            return None;
        }
        let request_input = Arc::clone(request_input);
        let stdin_tx = stdin_tx.clone();
        let task = tokio::spawn(async move {
            let question = permission_question(&req.request);
            // A dropped sender must deny, never allow.
            let answers = (request_input)(vec![question.clone()])
                .await
                .unwrap_or_default();
            let decision = answers
                .into_iter()
                .find(|answer| answer.question_id == question.id)
                .map(|answer| match answer.decision {
                    Some(decision) => decision,
                    None if answer
                        .labels
                        .iter()
                        .any(|label| label.eq_ignore_ascii_case(PERMISSION_ALLOW)) =>
                    {
                        allow_as_asked()
                    }
                    None => deny_and_stop(),
                })
                .unwrap_or_else(deny_and_stop);
            let tool_use_id = req.request.text("tool_use_id");
            let response = permission_response(&decision, &req.request.input, tool_use_id);
            let line = control_response_line(&req.request_id, response);
            stdin_tx.send(StdinMsg::Line(line)).ok();
        });
        return Some(task.abort_handle());
    }
    let request_input = Arc::clone(request_input);
    let stdin_tx = stdin_tx.clone();
    let task = tokio::spawn(async move {
        let request_id = req.request_id;
        let input = req.request.input;
        let questions = parse_questions(&input);
        // Don't emit InputRequested here: the engine's input bridge owns that lifecycle.
        let answers = (request_input)(questions.clone()).await.unwrap_or_default();
        let updated = updated_input_with_answers(&input, &questions, &answers);
        let line = control_response_line(&request_id, allow_response(updated));
        if stdin_tx.send(StdinMsg::Line(line)).is_err() {
            log::debug!("claude stdin writer gone before the AskUserQuestion answer was sent");
        }
    });
    Some(task.abort_handle())
}

/// A save must never hold a tool up for long.
const PRESAVE_TIMEOUT: Duration = Duration::from_secs(2);

fn answer_presave_hook(
    req: ControlRequestFrame,
    before_file_tool: &BeforeFileToolFn,
    stdin_tx: &mpsc::UnboundedSender<StdinMsg>,
) -> Option<tokio::task::AbortHandle> {
    let line = control_response_line(&req.request_id, serde_json::json!({ "continue": true }));
    let Some(path) = req
        .request
        .input
        .pointer("/tool_input/file_path")
        .and_then(Value::as_str)
    else {
        stdin_tx.send(StdinMsg::Line(line)).ok();
        return None;
    };
    let saved = before_file_tool(path.to_owned());
    let path = path.to_owned();
    let stdin_tx = stdin_tx.clone();
    let task = tokio::spawn(async move {
        if tokio::time::timeout(PRESAVE_TIMEOUT, saved).await.is_err() {
            log::warn!(
                "{path} wasn't saved within {PRESAVE_TIMEOUT:?}, so Claude uses it as it is on disk"
            );
        }
        if stdin_tx.send(StdinMsg::Line(line)).is_err() {
            log::debug!("claude stdin writer gone before the save hook was answered");
        }
    });
    Some(task.abort_handle())
}

/// The CLI waits on every MCP reply, so a lost one must not hold the tool call up.
const MCP_TIMEOUT: Duration = Duration::from_secs(10);

fn answer_mcp_message(
    req: ControlRequestFrame,
    mcp: &McpMessageFn,
    stdin_tx: &mpsc::UnboundedSender<StdinMsg>,
) -> Option<tokio::task::AbortHandle> {
    let message = req
        .request
        .other
        .get("message")
        .cloned()
        .unwrap_or_default();
    let id = message
        .get("id")
        .cloned()
        .unwrap_or_else(|| serde_json::json!(0));
    let reply = mcp(message);
    let request_id = req.request_id;
    let stdin_tx = stdin_tx.clone();
    let task = tokio::spawn(async move {
        let reply = match tokio::time::timeout(MCP_TIMEOUT, reply).await {
            Ok(Ok(reply)) => reply,
            Ok(Err(_)) | Err(_) => {
                log::warn!("wu's ide MCP server didn't answer within {MCP_TIMEOUT:?}");
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": { "code": -32603, "message": "Wu didn't answer this MCP request." },
                })
            }
        };
        let line = control_response_line(&request_id, serde_json::json!({ "mcp_response": reply }));
        if stdin_tx.send(StdinMsg::Line(line)).is_err() {
            log::debug!("claude stdin writer gone before an MCP message was answered");
        }
    });
    Some(task.abort_handle())
}

fn elicitation_question(request: &ControlRequestBody) -> Option<UserInputQuestion> {
    let is_form = match request.other.get("mode") {
        None | Some(Value::Null) => true,
        Some(mode) => mode.as_str() == Some("form"),
    };
    let has_fields = request
        .other
        .get("requested_schema")
        .and_then(|schema| schema.get("properties"))
        .and_then(Value::as_object)
        .is_some_and(|properties| !properties.is_empty());
    let message = request.text("message")?;
    if !is_form || has_fields {
        return None;
    }
    let header = request
        .text("display_name")
        .or_else(|| request.text("mcp_server_name"))
        .unwrap_or("Claude");
    Some(UserInputQuestion {
        id: uuid::Uuid::new_v4().to_string(),
        header: header.into(),
        question: message.into(),
        options: vec![t!("Allow").to_string(), t!("Deny").to_string()],
        multi_select: false,
        prefill: None,
        multiline: false,
        accepts_typed_answer: false,
        permission: None,
    })
}

fn answer_elicitation(
    req: ControlRequestFrame,
    request_input: &Arc<RequestInputFn>,
    stdin_tx: &mpsc::UnboundedSender<StdinMsg>,
) -> Option<tokio::task::AbortHandle> {
    let request_id = req.request_id;
    let Some(question) = elicitation_question(&req.request) else {
        log::warn!(
            "declining a {} question from {} that Wu can't show",
            req.request.text("mode").unwrap_or("form"),
            req.request
                .text("mcp_server_name")
                .unwrap_or("an unnamed tool")
        );
        let line = control_response_line(&request_id, serde_json::json!({ "action": "decline" }));
        stdin_tx.send(StdinMsg::Line(line)).ok();
        return None;
    };
    let request_input = Arc::clone(request_input);
    let stdin_tx = stdin_tx.clone();
    let task = tokio::spawn(async move {
        let answers = (request_input)(vec![question.clone()])
            .await
            .unwrap_or_default();
        let accept = answers.iter().any(|answer| {
            answer.question_id == question.id
                && answer.labels.iter().any(|label| {
                    question
                        .options
                        .first()
                        .is_some_and(|allow| allow.eq_ignore_ascii_case(label))
                })
        });
        let response = if accept {
            serde_json::json!({ "action": "accept", "content": {} })
        } else {
            serde_json::json!({ "action": "decline" })
        };
        let line = control_response_line(&request_id, response);
        if stdin_tx.send(StdinMsg::Line(line)).is_err() {
            log::debug!("claude stdin writer gone before the elicitation answer was sent");
        }
    });
    Some(task.abort_handle())
}

fn parse_questions(input: &Value) -> Vec<UserInputQuestion> {
    let raw = input.get("questions").and_then(Value::as_array);
    raw.map(|a| a.as_slice())
        .unwrap_or_default()
        .iter()
        .map(|q| {
            let field =
                |keys: [&str; 2]| keys.iter().find_map(|k| q.get(*k).and_then(Value::as_str));
            UserInputQuestion {
                id: uuid::Uuid::new_v4().to_string(),
                header: field(["header", "title"]).unwrap_or(QUESTION_HEADER).into(),
                question: field(["question", "prompt"]).unwrap_or("").into(),
                prefill: None,
                multiline: false,
                accepts_typed_answer: true,
                multi_select: ["multiSelect", "multi_select"]
                    .iter()
                    .find_map(|k| q.get(*k).and_then(Value::as_bool))
                    .unwrap_or(false),
                options: q
                    .get("options")
                    .and_then(Value::as_array)
                    .map(|a| a.as_slice())
                    .unwrap_or_default()
                    .iter()
                    .map(|op| match op {
                        Value::String(s) => s.clone(),
                        other => other
                            .get("label")
                            .or_else(|| other.get("value"))
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .into(),
                    })
                    .collect(),
                permission: None,
            }
        })
        .collect()
}

fn updated_input_with_answers(
    input: &Value,
    questions: &[UserInputQuestion],
    answers: &[UserInputAnswer],
) -> Value {
    let mut updated = match input {
        Value::Object(map) => map.clone(),
        _ => serde_json::Map::new(),
    };
    let mut by_question = serde_json::Map::new();
    for q in questions {
        let labels: Vec<String> = answers
            .iter()
            .find(|a| a.question_id == q.id)
            .map(|a| a.labels.clone())
            .unwrap_or_default();
        let value = if q.multi_select {
            Value::Array(labels.into_iter().map(Value::String).collect())
        } else {
            Value::String(labels.into_iter().next().unwrap_or_default())
        };
        by_question.insert(q.question.clone(), value);
    }
    updated.insert("answers".into(), Value::Object(by_question));
    Value::Object(updated)
}

fn flag_settings(request: &RunRequest) -> serde_json::Map<String, Value> {
    let mut settings = serde_json::Map::new();
    if option_is_on(&request.model_options, "fastMode") {
        settings.insert("fastMode".into(), Value::Bool(true));
    }
    // A stored "on" must beat a user `alwaysThinkingEnabled: false`, which flag settings override.
    if option_is_on(&request.model_options, catalog::THINKING_OPTION) {
        settings.insert("alwaysThinkingEnabled".into(), Value::Bool(true));
    }
    if request.reasoning == Some(ReasoningLevel::Ultracode) {
        settings.insert("ultracode".into(), Value::Bool(true));
    }
    if let Some(tokens) = request.auto_compact_tokens {
        settings.insert("autoCompactEnabled".into(), Value::Bool(true));
        settings.insert("autoCompactWindow".into(), tokens.into());
    }
    settings
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_stored_thinking_on_is_forced_at_launch() {
        let settings = |options: serde_json::Value| {
            let launch = RunRequest {
                prompt: String::new(),
                model: None,
                reasoning: None,
                model_options: options.as_object().cloned().unwrap_or_default(),
                cwd: String::new(),
                permission: PermissionMode::default(),
                resume: None,
                fork: None,
                attachments: Vec::new(),
                skills: Vec::new(),
                auto_compact_tokens: None,
                chrome: false,
                drop_prompt: None,
                ephemeral: false,
                context: Vec::new(),
            };
            flag_settings(&launch).get("alwaysThinkingEnabled").cloned()
        };
        assert_eq!(
            settings(json!({ "thinking": "on" })),
            Some(Value::Bool(true))
        );
        assert_eq!(settings(json!({ "thinking": "off" })), None);
        assert_eq!(settings(json!({})), None);
    }

    #[test]
    fn pasted_code_is_split_at_hash() {
        let authenticate =
            json!({ "manualUrl": "https://claude.example/manual?code=true&state=from-url" });
        assert_eq!(
            pasted_code(" abc#xyz\n", &authenticate),
            ("abc".to_owned(), "xyz".to_owned())
        );
        assert_eq!(
            pasted_code("abc", &authenticate),
            ("abc".to_owned(), "from-url".to_owned())
        );
        // Like the extension, a code without a state still goes to the CLI, which explains a refusal.
        assert_eq!(
            pasted_code("abc", &json!({})),
            ("abc".to_owned(), String::new())
        );
    }

    async fn answer_to(
        raw: Value,
        request_input: &Arc<RequestInputFn>,
        stdin_tx: &mpsc::UnboundedSender<StdinMsg>,
        stdin_rx: &mut mpsc::UnboundedReceiver<StdinMsg>,
    ) -> Value {
        let request: ControlRequestFrame = serde_json::from_value(raw).unwrap();
        handle_control_request(
            request,
            false,
            request_input,
            &crate::file_tools_go_ahead(),
            &crate::no_mcp_servers(),
            stdin_tx,
        );
        let reply = tokio::time::timeout(std::time::Duration::from_secs(5), stdin_rx.recv());
        match reply.await {
            Ok(Some(StdinMsg::Line(line))) => serde_json::from_str(&line).unwrap(),
            _ => panic!("the request was never answered"),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_save_that_never_finishes_lets_the_tool_go_ahead() {
        let (stdin_tx, mut stdin_rx) = mpsc::unbounded_channel();
        let held = Arc::new(std::sync::Mutex::new(Vec::new()));
        let before_file_tool: BeforeFileToolFn = Arc::new({
            let held = held.clone();
            move |_| {
                let (saved_tx, saved_rx) = tokio::sync::oneshot::channel();
                held.lock().unwrap().push(saved_tx);
                saved_rx
            }
        });
        let request: ControlRequestFrame = serde_json::from_value(json!({
            "request_id": "h1",
            "request": {
                "subtype": "hook_callback",
                "callback_id": wire::PRESAVE_CALLBACK_ID,
                "input": {"tool_name": "Read", "tool_input": {"file_path": "/x/a.rs"}},
            },
        }))
        .unwrap();
        let request_input: Arc<RequestInputFn> =
            Arc::new(Box::new(|_| tokio::sync::oneshot::channel().1));
        handle_control_request(
            request,
            false,
            &request_input,
            &before_file_tool,
            &crate::no_mcp_servers(),
            &stdin_tx,
        );

        let started = tokio::time::Instant::now();
        let Some(StdinMsg::Line(line)) = stdin_rx.recv().await else {
            panic!("the hook was never answered");
        };
        assert_eq!(started.elapsed(), PRESAVE_TIMEOUT);
        let reply: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(reply["response"]["request_id"], "h1");
        assert_eq!(reply["response"]["response"], json!({"continue": true}));
        assert_eq!(held.lock().unwrap().len(), 1);
    }

    #[test]
    fn unreadable_control_requests_still_yield_their_id() {
        let broken = r#"{"type":"control_request","request_id":"r9","request":{"subtype":"can_use_tool","tool_name":null}}"#;
        assert!(wire::parse_frame(broken).is_err());
        assert_eq!(
            wire::unreadable_control_request_id(broken).as_deref(),
            Some("r9")
        );
        assert_eq!(
            wire::unreadable_control_request_id(r#"{"type":"assistant","request_id":"x"}"#),
            None
        );
    }

    #[tokio::test]
    async fn every_control_request_gets_an_answer() {
        let (stdin_tx, mut stdin_rx) = mpsc::unbounded_channel();
        let asked = Arc::new(std::sync::Mutex::new(Vec::<UserInputQuestion>::new()));
        let request_input: Arc<RequestInputFn> = Arc::new(Box::new({
            let asked = asked.clone();
            move |questions: Vec<UserInputQuestion>| {
                let (answer_tx, answer_rx) = tokio::sync::oneshot::channel();
                let answers = questions
                    .iter()
                    .map(|question| UserInputAnswer {
                        question_id: question.id.clone(),
                        labels: vec![PERMISSION_ALLOW.into()],
                        decision: None,
                    })
                    .collect();
                asked.lock().unwrap().extend(questions);
                answer_tx.send(answers).ok();
                answer_rx
            }
        }));
        let mut answer =
            async |raw: Value| answer_to(raw, &request_input, &stdin_tx, &mut stdin_rx).await;

        let unknown =
            answer(json!({"request_id": "r1", "request": {"subtype": "mcp_message"}})).await;
        assert_eq!(unknown["response"]["subtype"], "error");
        assert_eq!(unknown["response"]["request_id"], "r1");

        let confirm = answer(json!({"request_id": "r2", "request": {
            "subtype": "elicitation",
            "mcp_server_name": "claude-in-chrome",
            "message": "Allow Claude to use example.com?",
            "mode": "form",
            "requested_schema": {"type": "object", "properties": {}},
        }}))
        .await;
        assert_eq!(confirm["response"]["request_id"], "r2");
        assert_eq!(
            confirm["response"]["response"],
            json!({"action": "accept", "content": {}})
        );

        let link = answer(json!({"request_id": "r3", "request": {
            "subtype": "elicitation",
            "mcp_server_name": "linear",
            "message": "Sign in to continue",
            "mode": "url",
            "url": "https://example.com/auth",
        }}))
        .await;
        assert_eq!(link["response"]["response"], json!({"action": "decline"}));

        let form = answer(json!({"request_id": "r4", "request": {
            "subtype": "elicitation",
            "mcp_server_name": "notes",
            "message": "Pick a folder",
            "requested_schema": {"type": "object", "properties": {"folder": {"type": "string"}}},
        }}))
        .await;
        assert_eq!(form["response"]["response"], json!({"action": "decline"}));

        let odd = answer(json!({"request_id": "r5", "request": {
            "subtype": "elicitation",
            "mcp_server_name": null,
            "message": "Continue?",
            "mode": 7,
        }}))
        .await;
        assert_eq!(odd["response"]["response"], json!({"action": "decline"}));

        let asked = asked.lock().unwrap();
        assert_eq!(asked.len(), 1, "only the plain confirmation is shown");
        assert_eq!(asked[0].header, "claude-in-chrome");
        assert_eq!(asked[0].question, "Allow Claude to use example.com?");
    }

    #[tokio::test]
    async fn permission_prompt_carries_suggestions() {
        let (stdin_tx, mut stdin_rx) = mpsc::unbounded_channel();
        let asked = Arc::new(std::sync::Mutex::new(Vec::<UserInputQuestion>::new()));
        let request_input: Arc<RequestInputFn> = Arc::new(Box::new({
            let asked = asked.clone();
            move |questions: Vec<UserInputQuestion>| {
                let (answer_tx, answer_rx) = tokio::sync::oneshot::channel();
                asked.lock().unwrap().extend(questions);
                answer_tx.send(Vec::new()).ok();
                answer_rx
            }
        }));
        answer_to(
            json!({"request_id": "r1", "request": {
                "subtype": "can_use_tool",
                "tool_name": "Bash",
                "input": {"command": "ls"},
                "tool_use_id": "t1",
                "permission_suggestions": [{
                    "type": "addRules",
                    "rules": [{"toolName": "Bash", "ruleContent": "ls"}],
                    "behavior": "allow",
                    "destination": "localSettings",
                }],
            }}),
            &request_input,
            &stdin_tx,
            &mut stdin_rx,
        )
        .await;
        let asked = asked.lock().unwrap();
        let permission = asked[0].permission.as_ref().expect("a typed prompt");
        assert_eq!(permission.suggestions.len(), 1);
        assert!(permission.allows_always);
        assert_eq!(permission.tool_use_id.as_deref(), Some("t1"));
    }

    #[tokio::test]
    async fn dropped_permission_prompt_denies() {
        let (stdin_tx, mut stdin_rx) = mpsc::unbounded_channel();
        let request_input: Arc<RequestInputFn> = Arc::new(Box::new(|_| {
            let (_answer_tx, answer_rx) = tokio::sync::oneshot::channel();
            answer_rx
        }));
        let reply = answer_to(
            json!({"request_id": "r1", "request": {
                "subtype": "can_use_tool",
                "tool_name": "Bash",
                "input": {"command": "rm -rf build"},
                "tool_use_id": "t1",
            }}),
            &request_input,
            &stdin_tx,
            &mut stdin_rx,
        )
        .await;
        let response = &reply["response"]["response"];
        assert_eq!(response["behavior"], "deny");
        assert_eq!(response["message"], DENY_STOP);
        assert_eq!(response["toolUseID"], "t1");
    }

    #[test]
    fn auto_compact_limit_turns_on_claude_auto_compaction_at_that_window() {
        let mut request = RunRequest {
            prompt: "hi".into(),
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
            context: Vec::new(),
        };
        assert!(flag_settings(&request).is_empty());
        request.auto_compact_tokens = Some(150_000);
        assert_eq!(
            Value::Object(flag_settings(&request)),
            json!({ "autoCompactEnabled": true, "autoCompactWindow": 150_000 })
        );
    }

    #[test]
    fn chrome_is_connected_only_when_asked() {
        let mut request = RunRequest {
            prompt: "hi".into(),
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
            chrome: true,
            drop_prompt: None,
            ephemeral: false,
            context: Vec::new(),
        };
        let harness = ClaudeHarness::new();
        let args = |request: &RunRequest| -> Vec<String> {
            harness
                .build_command(&PathBuf::from("claude"), request, None)
                .as_std()
                .get_args()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect()
        };
        let with_chrome = args(&request);
        assert!(with_chrome.contains(&"--chrome".to_string()));
        assert!(!with_chrome.contains(&"--no-chrome".to_string()));
        request.chrome = false;
        let without_chrome = args(&request);
        assert!(without_chrome.contains(&"--no-chrome".to_string()));
        assert!(!without_chrome.contains(&"--chrome".to_string()));
    }

    #[test]
    fn build_command_enables_file_checkpointing() {
        let request = RunRequest {
            prompt: "hi".into(),
            model: None,
            reasoning: None,
            model_options: serde_json::Map::new(),
            cwd: String::new(),
            permission: PermissionMode::Ask,
            resume: None,
            fork: None,
            attachments: Vec::new(),
            skills: Vec::new(),
            auto_compact_tokens: None,
            chrome: false,
            drop_prompt: None,
            ephemeral: false,
            context: Vec::new(),
        };
        let command = ClaudeHarness::new().build_command(&PathBuf::from("claude"), &request, None);
        let value = command
            .as_std()
            .get_envs()
            .find(|(key, _)| *key == "CLAUDE_CODE_ENABLE_SDK_FILE_CHECKPOINTING")
            .and_then(|(_, value)| value);
        assert_eq!(value, Some(std::ffi::OsStr::new("true")));
    }

    #[test]
    fn chat_runs_never_inherit_an_ide_port() {
        let request = RunRequest {
            prompt: "hi".into(),
            model: None,
            reasoning: None,
            model_options: serde_json::Map::new(),
            cwd: String::new(),
            permission: PermissionMode::Ask,
            resume: None,
            fork: None,
            attachments: Vec::new(),
            skills: Vec::new(),
            auto_compact_tokens: None,
            chrome: false,
            drop_prompt: None,
            ephemeral: false,
            context: Vec::new(),
        };
        let command = ClaudeHarness::new().build_command(&PathBuf::from("claude"), &request, None);
        assert!(
            command
                .as_std()
                .get_envs()
                .any(|(key, value)| key == "CLAUDE_CODE_SSE_PORT" && value.is_none()),
            "the port is removed from the inherited environment"
        );
    }

    #[test]
    fn ephemeral_runs_keep_no_saved_session() {
        let mut request = RunRequest {
            prompt: "hi".into(),
            model: None,
            reasoning: None,
            model_options: serde_json::Map::new(),
            cwd: String::new(),
            permission: PermissionMode::Ask,
            resume: None,
            fork: None,
            attachments: Vec::new(),
            skills: Vec::new(),
            auto_compact_tokens: None,
            chrome: false,
            drop_prompt: None,
            ephemeral: false,
            context: Vec::new(),
        };
        let harness = ClaudeHarness::new();
        let persists = |request: &RunRequest| {
            !harness
                .build_command(&PathBuf::from("claude"), request, None)
                .as_std()
                .get_args()
                .any(|arg| arg == "--no-session-persistence")
        };
        assert!(persists(&request));
        request.ephemeral = true;
        assert!(!persists(&request));
    }

    #[test]
    fn commands_the_user_or_project_define_are_never_treated_as_hidden() {
        let config = tempfile::tempdir().expect("config directory");
        let workspace = tempfile::tempdir().expect("workspace directory");
        let project = workspace.path().join("app");
        let write = |path: std::path::PathBuf| {
            std::fs::create_dir_all(path.parent().expect("parent")).expect("create directory");
            std::fs::write(path, "body").expect("write file");
        };
        write(config.path().join("commands/usage.md"));
        write(workspace.path().join(".claude/commands/team/status.md"));
        write(project.join(".claude/skills/memory/SKILL.md"));
        write(project.join(".claude/skills/context/notes.md"));
        write(project.join(".claude/commands/model.txt"));

        let names = defined_command_names(&project, config.path());
        for name in ["usage", "status", "memory"] {
            assert!(names.contains(name), "{name}");
        }
        for name in ["context", "model", "notes"] {
            assert!(!names.contains(name), "{name}");
        }
    }

    #[test]
    fn picked_skills_become_an_instruction_to_use_them() {
        let skill = |name: &str| SkillRef {
            name: name.into(),
            path: format!("claude-skill:{name}"),
        };
        assert_eq!(with_skills("no skills", &[]), "no skills");
        assert_eq!(
            with_skills(
                "$review and $docs",
                &[skill("review"), skill("docs"), skill("review")]
            ),
            "$review and $docs\n\nBefore you start, use these skills with the `Skill` tool: `review`, `docs`."
        );
    }

    #[test]
    fn skills_from_projects_users_and_plugins_are_recognized() {
        let config = tempfile::tempdir().expect("config directory");
        let project = tempfile::tempdir().expect("project directory");
        let write = |path: std::path::PathBuf| {
            std::fs::create_dir_all(path.parent().expect("parent")).expect("create directory");
            std::fs::write(path, "body").expect("write file");
        };
        write(project.path().join(".claude/skills/deploy/SKILL.md"));
        write(project.path().join(".claude/commands/status.md"));
        write(config.path().join("skills/notes/SKILL.md"));
        write(
            config
                .path()
                .join("plugins/synced/abc/marketing/skills/seo-audit/SKILL.md"),
        );
        write(
            config
                .path()
                .join("plugins/cache/market/review/1.0.0/skills/check/SKILL.md"),
        );
        write(
            config
                .path()
                .join("plugins/cache/market/review/1.0.0/commands/ship.md"),
        );

        let skills = defined_skill_names(project.path(), config.path());
        for name in ["deploy", "notes", "marketing:seo-audit", "review:check"] {
            assert!(skills.contains(name), "{name}");
        }
        for name in ["status", "review:ship", "seo-audit"] {
            assert!(!skills.contains(name), "{name}");
        }
    }

    #[test]
    fn each_permission_mode_maps_to_its_cli_flags() {
        assert_eq!(
            permission_args(PermissionMode::Auto),
            ["--permission-mode", "auto"]
        );
        assert_eq!(
            permission_args(PermissionMode::Ask),
            ["--permission-mode", "default"]
        );
        assert_eq!(
            permission_args(PermissionMode::AcceptEdits),
            ["--permission-mode", "acceptEdits"]
        );
        assert_eq!(
            permission_args(PermissionMode::DontAsk),
            ["--permission-mode", "dontAsk"]
        );
        assert_eq!(
            permission_args(PermissionMode::FullAccess),
            [
                "--permission-mode",
                "bypassPermissions",
                "--dangerously-skip-permissions"
            ]
        );
        assert_eq!(
            permission_args(PermissionMode::ReadOnly),
            ["--permission-mode", "auto"],
            "Codex-only modes fall back to auto"
        );
    }

    #[test]
    fn plan_mode_uses_the_plan_flag() {
        assert_eq!(
            permission_args(PermissionMode::Plan),
            ["--permission-mode", "plan"]
        );
        assert_eq!(permission_mode_name(PermissionMode::Plan), "plan");
    }

    #[test]
    fn reported_mode_names_map_back_to_modes() {
        for mode in PermissionMode::choices(HarnessId::ClaudeCode) {
            assert_eq!(
                permission_mode_from_name(permission_mode_name(*mode)),
                Some(*mode)
            );
        }
        assert_eq!(
            permission_mode_from_name("default"),
            Some(PermissionMode::Ask)
        );
        assert_eq!(permission_mode_from_name("unknown"), None);
    }

    #[test]
    fn plan_approval_asks_ready_to_code() {
        let request: ControlRequestBody = serde_json::from_value(json!({
            "subtype": "can_use_tool",
            "tool_name": "ExitPlanMode",
            "input": {"plan": "# Plan"},
        }))
        .expect("a can_use_tool request");
        assert_eq!(permission_question(&request).question, "Ready to code?");
    }

    #[test]
    fn live_mode_changes_match_the_launch_flags() {
        for mode in [
            PermissionMode::Auto,
            PermissionMode::Ask,
            PermissionMode::Plan,
            PermissionMode::AcceptEdits,
            PermissionMode::DontAsk,
            PermissionMode::FullAccess,
            PermissionMode::ReadOnly,
            PermissionMode::ApproveForMe,
        ] {
            assert_eq!(
                permission_args(mode)[1],
                permission_mode_name(mode),
                "{mode:?}"
            );
        }
        assert_eq!(
            live_permission_mode_name(PermissionMode::FullAccess, true),
            "bypassPermissions"
        );
        assert_eq!(
            live_permission_mode_name(PermissionMode::FullAccess, false),
            "default"
        );
    }

    #[test]
    fn parses_questions_tolerantly() {
        let input = json!({
            "questions": [
                {
                    "header": "Choice",
                    "question": "Pick one",
                    "options": ["A", {"label": "B", "description": "second"}],
                    "multiSelect": false
                },
                { "title": "Alt", "prompt": "Pick many", "multi_select": true }
            ]
        });
        let qs = parse_questions(&input);
        assert_eq!(qs.len(), 2);
        assert_eq!(qs[0].header, "Choice");
        assert_eq!(qs[0].options, vec!["A".to_string(), "B".to_string()]);
        assert!(!qs[0].multi_select);
        assert_eq!(qs[1].header, "Alt");
        assert_eq!(qs[1].question, "Pick many");
        assert!(qs[1].multi_select);
    }

    #[test]
    fn answers_key_by_question_text() {
        let input =
            json!({"questions": [{"header": "H", "question": "Pick one", "options": ["A", "B"]}]});
        let qs = parse_questions(&input);
        let answers = vec![UserInputAnswer {
            question_id: qs[0].id.clone(),
            labels: vec!["B".into()],
            decision: None,
        }];
        let updated = updated_input_with_answers(&input, &qs, &answers);
        assert_eq!(updated["answers"]["Pick one"], json!("B"));
        assert!(updated["questions"].is_array());
    }

    #[test]
    fn image_media_type_prefers_file_contents_over_extension() {
        let path = std::path::Path::new;
        assert_eq!(
            image_media_type(path("renamed.png"), b"\xFF\xD8\xFFjpeg"),
            Some("image/jpeg")
        );
    }

    #[test]
    fn image_media_type_falls_back_to_extension_then_magic_bytes() {
        let path = std::path::Path::new;
        assert_eq!(image_media_type(path("a.PNG"), b""), Some("image/png"));
        assert_eq!(image_media_type(path("a.jpg"), b""), Some("image/jpeg"));
        assert_eq!(image_media_type(path("a.gif"), b""), Some("image/gif"));
        assert_eq!(image_media_type(path("a.webp"), b""), Some("image/webp"));
        assert_eq!(
            image_media_type(path("paste"), b"\x89PNG\r\n"),
            Some("image/png")
        );
        assert_eq!(
            image_media_type(path("paste"), b"RIFF\0\0\0\0WEBPVP8"),
            Some("image/webp")
        );
        assert_eq!(image_media_type(path("a.svg"), b"<svg"), None);
    }

    #[tokio::test]
    async fn pdf_attachment_is_a_document_block() {
        let directory = tempfile::tempdir().unwrap();
        let pdf = directory.path().join("spec.pdf");
        std::fs::write(&pdf, b"%PDF-1.7").unwrap();
        let (blocks, problems) =
            load_attachment_blocks(&[pdf.to_string_lossy().into_owned()]).await;
        assert!(problems.is_empty(), "{problems:?}");
        let line: Value =
            serde_json::from_str(&wire::user_message_line("read it", &blocks, &[], "u1")).unwrap();
        assert_eq!(
            line["message"]["content"][0],
            json!({
                "type": "document",
                "source": {"type": "base64", "media_type": "application/pdf", "data": "JVBERi0xLjc="},
                "title": "spec.pdf",
            })
        );
    }

    #[tokio::test]
    async fn oversized_pdf_reports_a_problem_and_is_skipped() {
        let directory = tempfile::tempdir().unwrap();
        let pdf = directory.path().join("huge.pdf");
        std::fs::File::create(&pdf)
            .unwrap()
            .set_len(MAX_PDF_BYTES + 1)
            .unwrap();
        let (blocks, problems) =
            load_attachment_blocks(&[pdf.to_string_lossy().into_owned()]).await;
        assert!(blocks.is_empty());
        assert_eq!(
            problems,
            ["huge.pdf wasn't sent: Claude only accepts PDFs up to 32 MB."]
        );
    }
}
