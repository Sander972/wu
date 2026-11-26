# Claude Code VS Code Extension Parity Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Bring wu's Claude agent chat to feature parity with the official Claude Code extension for VS Code (v2.1.331), with a verification for every feature.

**Architecture:** wu already drives the `claude` CLI over the stream-json SDK control protocol, the same channel the extension uses. Almost every missing feature is either (a) a control request wu does not send yet, (b) a richer answer to `can_use_tool`, or (c) editor-side context wu does not collect. So the plan adds two foundations first (a generic request/response control channel and a typed permission decision), then builds each feature as a thin layer on top. Nothing is copied from the extension; only its wire shapes are matched so the same CLI behaves the same.

**Tech Stack:** Rust, GPUI, tokio (harness), `async-tungstenite` (already a workspace dependency), the user's installed `claude` CLI (2.1.332 at `C:\Users\Utente\.local\bin\claude.exe`).

**Spec:** There is no separate design doc: the user asked for the plan directly. The spec is the **Parity Matrix** at the end of this file plus the **Protocol Reference** section. Sources: `https://code.claude.com/docs/en/vs-code`, the installed extension at `~/.vscode/extensions/anthropic.claude-code-2.1.331-win32-x64/` (`package.json`, `extension.js`, `webview/index.js`), and `claude --help`.

## Progress (2026-11-27)

- Done and reviewed: **all 50 tasks** (Phases 0-10), every phase-end check, and the final whole-branch review with its fix wave. Open: Task 50 Steps 2-3 (the side-by-side GUI walkthrough against VS Code), left to a human. See `claude-parity_hand-off.md` at the repo root.
- Additional rulings since Task 32: Task 33's first auto-archive pass waits 60 s after launch; Task 48 plugin links open without a confirm prompt (like reply links); Phase 10 uses `surface: "vscode"` by the user's decision (temporary, one `SURFACE` constant); every commit also passes a non-test `cargo check`.
- The executor's ledger (git-ignored) is `.superpowers/sdd/2026-11-25-claude-vscode-parity/progress.md`: it holds every ruling, deferred minor finding and the list of manual GUI checks still to run.
- Rulings that change this plan's text:
  - `HIDDEN_COMMANDS` hides the CLI's built-in commands, and a visible CLI command with the same name pushes wu's native command to `/wu:<name>`. Wherever a task says "remove `<name>` from `HIDDEN_COMMANDS`", do the opposite: every native `AppCommand` name must be in the list (enforced by the test `claude_never_shadows_a_wu_command`). Task 32's `no_native_command_is_hidden` test is replaced by that test.
  - Task 25: `claude mcp add/remove` use the extension's argument order (`--scope S --transport T -- name target...`, stdio commands split on whitespace).
  - Task 26: permission rules are edited with `claude edit-permission-rules --json` on stdin, because `update_settings` rejects the `permissions` key.
  - Task 14: forks keep wu's existing title scheme `"<title> (2)"`.
- Carry-overs: Task 40 adds "Focus view" to the command menu's Settings section; Task 44 replaces the `/remote-control` toast.
- Known pre-existing issues, not from this plan: clippy `redundant_clone` at `crates/agent_harness/src/usage.rs:171`, a clippy warning at `crates/workspace/src/status_bar.rs:220`, and the flaky `agent_chat` test `send_now_shows_the_message_and_queue_order_survives_idle` (fails under full-suite load, passes alone).

## Global Constraints

- Edit in the main checkout on `main`. No worktrees, no feature branches, never `git push`, never amend or rebase. One local commit per task. No AI attribution lines in commit messages.
- Never hard-wrap prose, comments, docstrings or commit messages.
- Rust rules from `AGENTS.md`: no `unwrap()`, no `let _ =` on fallible calls, no `mod.rs`, full-word variable names, comments only for a non-obvious why, no new files unless it is a new logical component.
- Format with `rustfmt --edition 2024 <file>` on touched files only. Never `cargo fmt -p`.
- Lint with `./script/clippy -p <crate>` (PowerShell: `script/clippy.ps1`), once per phase (see Phase-end check), not per task.
- Every new user-facing string needs zh-CN text: `t!` strings in `assets/locales/zh-CN/strings/<crate>.json` (key-sorted), settings titles in `assets/locales/zh-CN/strings/settings_ui.json`, settings doc comments in `assets/locales/zh-CN/schema/settings.json`. Check with `cargo test -p i18n`, `cargo test -p settings --lib`, `cargo test -p settings_ui --lib`. Edit those JSON files with the Edit tool, not a Python heredoc.
- **Settings recipe** (referenced by tasks as "add setting `x`"): (1) `Option<T>` field with `///` doc and `Default:` line in `AgentChatSettingsContent` (`crates/settings_content/src/settings_content.rs:1086-1180`), (2) field in `AgentChatSettings` and `from_settings` (`crates/agent_chat/src/agent_chat_settings.rs`), (3) `assets/settings/default.json` `"agent_chat"` block, (4) a `SettingItem` in `agent_chat_page()` (`crates/settings_ui/src/page_data.rs:7762`) and bump that section's array length, (5) the three zh-CN files.
- New keybindings go in all three `assets/keymaps/default-*.json`; `cmd-` only in the macOS file (`script/check-keymaps`).
- Other agents (Codex, Opencode, Antigravity) must keep working: every new `Harness` trait method has a default body, every new struct field is `#[serde(default)]`.
- Do not set `CLAUDE_CODE_ENTRYPOINT=claude-vscode` or otherwise identify wu as the VS Code extension.
- Drive E: fills up during builds. If a build fails on disk space, run `cargo trim` and rebuild.
- **Task routine.** Every task runs in this order: write the listed tests, run them and see them fail, implement, run them and see them pass, run the task's manual check in the dev app (`cargo run -p wu`) when one is listed, `rustfmt` touched files, commit. The steps below list only what is specific to the task.
- **Narrow per-task testing.** Within a task, run only the tests for the code it touched (`cargo test -p <crate> <filter>`), plus `cargo test -p i18n`, `cargo test -p settings --lib` and `cargo test -p settings_ui --lib` only when the task touched strings or settings. Do not run a whole crate suite or clippy per task. A "Verify" step that names a whole crate means the focused filter for that task.
- **Phase-end check.** After the last task of each phase, run once: `cargo test -p agent_harness`, `cargo test -p agent_chat`, the suites of any other crate the phase touched, `cargo test -p i18n`, `cargo test -p settings --lib`, `cargo test -p settings_ui --lib`, `./script/clippy -p agent_harness`, `./script/clippy -p agent_chat` and `script/check-keymaps`. Fix every failure in its own commit before the next phase starts.
- **Model choice.** Implement with the faster model (Sonnet) by default. Use the most capable model (Opus) for tasks that carry a Review Focus item, touch concurrency, the IDE server, or the plugin-drawing protocol (Tasks 46-49), and for any fix round a Sonnet implementer could not close.
- **Overlap.** The next task's implementer may start while the previous task is still in review; reviewers only read diffs and do not build. A fix round for the reviewed task lands as its own commit after whatever is in progress, never by amending.

## Review Focus

1. **A permission prompt whose UI disappears** (chat closed, window closed, app quitting, run interrupted): the tool must be denied, never allowed. Pinned in Task 3 (`dropped_permission_prompt_denies`).
2. **A file changed on disk between the proposed edit and Accept**: the accepted edit must not overwrite the newer content silently; wu re-reads, and if the original no longer matches it denies with a message telling Claude to re-read the file. Pinned in Task 8 (`stale_original_denies_instead_of_overwriting`).
3. **Rewind on a message with no checkpoint, or while a turn is running**: nothing on disk changes and the user sees why. Pinned in Task 13 (`rewind_refused_when_cannot_rewind`).
4. **An IDE-server connection with a missing or wrong token, and a lock file left by a crashed wu**: the socket closes with 1008 and stale locks are removed at startup. Pinned in Task 22 (`rejects_wrong_token`, `removes_stale_lock_files`).
5. **An older CLI that answers a control request with an error, or never answers**: the dialog shows the error and closes cleanly instead of hanging. Pinned in Task 2 (`control_error_and_timeout_surface`).
6. **A hostile deep link** (`session` containing `..` or a path separator, a `prompt` that tries to auto-run): rejected or only prefilled, never sent. Pinned in Task 41 (`open_link_validates_session_and_never_sends`).
7. **A plugin that sends a malformed, huge or unknown tree, or a link to a non-web address**: the chat keeps rendering, unknown parts show as plain text, and nothing but `http`/`https` is opened. Pinned in Task 47 (`unknown_node_kinds_become_text_not_errors`, `oversized_and_deeply_nested_trees_are_capped`) and Task 48 (`non_http_hrefs_are_not_opened`).

## Protocol Reference (shapes the tasks rely on)

All control requests wu sends use `{"type":"control_request","request_id":"wu-ctl-<uuid>","request":{"subtype":…}}` and are answered by `{"type":"control_response","response":{"subtype":"success"|"error","request_id":…,"response":{…}|"error":"…"}}`.

| subtype | request fields | response fields |
|---|---|---|
| `rewind_files` | `user_message_id`, `dry_run?` | `canRewind`, `filesChanged[]`, `insertions`, `deletions`, `error?` |
| `get_plan` | none | `exists`, `content`, `path` |
| `set_max_thinking_tokens` | `max_thinking_tokens` (31999 or 0), `thinking_display` | none |
| `mcp_status` | none | `mcpServers[{name,status,error?,scope?,tools?[]}]`, status in `connected/failed/needs-auth/pending/disabled` |
| `mcp_toggle` | `serverName`, `enabled` | none |
| `mcp_reconnect` | `serverName` | none |
| `mcp_authenticate` | `serverName` | `authUrl`, `requiresUserAction`, `error` |
| `mcp_clear_auth` | `serverName` | `error?` |
| `stop_task` | `task_id` | none |
| `get_task_output` | `task_id` | `output`, `total_bytes`, `truncated` |
| `background_tasks` | `tool_use_id` | `backgrounded` |
| `side_question` | `question` | `response` |
| `export_conversation` | none | `text`, `default_filename` |
| `get_usage` | none | `rate_limits{five_hour,seven_day,…{utilization,resets_at}}`, `subscription_type`, `session{total_cost_usd,total_duration_ms,total_lines_added,total_lines_removed,model_usage}` |
| `get_context_usage` | none | `model`, `totalTokens`, `rawMaxTokens`, `percentage`, `categories[{name,tokens}]`, `memoryFiles[{path,tokens}]` |
| `get_status` | none | `sections[{title,rows[{label?,value}]}]` |
| `get_settings` | none | `effective`, `sources[{source,settings}]` |
| `apply_flag_settings` | `settings{viewMode,outputStyle,remoteControlAtStartup,…}` | none |
| `update_settings` | `source` (`userSettings`/`localSettings`), `settings` | none |
| `list_permission_rules` | none | `state{rules[{behavior,source,rule,description{prefix,emphasis?,suffix?},editability}],workspaceDirectories[]}` |
| `get_hooks_listing` | none | `hooks[{event,matcher,source,pluginName?}]`, `events[{name,summary}]` |
| `get_memory_dialog` | none | `files[{kind,path,label,description,exists}]`, `memories[]`, `auto_memory{enabled,toggleable,status}` |
| `get_skills_dialog` | none | `skills[{name,display_name,state,source,tokens}]` |
| `get_sandbox_dialog` | none | `supported`, `enabled`, `mode`, `excluded_commands[]`, `restrictions` |
| `reload_plugins` | none | `commands`, `error_count` |
| `rename_session` | `title`, `source:"host"`, `session_id` | none |
| `generate_session_title` | `description`, `persist:true` | `title` |
| `claude_authenticate` | `loginWithClaudeAi` | `manualUrl`, `automaticUrl` |
| `claude_oauth_callback` | `authorizationCode`, `state` | none |
| `claude_oauth_wait_for_completion` | none | optional |
| `set_prompt_suggestions_paused` | `paused` | none |
| `remote_control` | `enabled`, `name?` | `session_url`, `connect_url` |
| `list_cloud_sessions` | `user_initiated?` | `sessions[{id,updated_at,title,branch?,repository?}]` |
| `get_cloud_session_transcript` | `session_id`, `cursor?` | `entries[]`, `next_cursor?` |

`can_use_tool` request fields: `tool_name`, `input`, `tool_use_id`, `permission_suggestions[]`, `suppress_always_allow_rule`, `blocked_path`, `title`, `description`, `plugin`. Reply: `{behavior:"allow",updatedInput,updatedPermissions:[],toolUseID}` or `{behavior:"deny",message,interrupt,toolUseID}`. Suggestion shapes: `{type:"addRules",rules:[{toolName,ruleContent?}],behavior,destination}`, `{type:"addDirectories",directories,destination}`, `{type:"setMode",mode,destination}`; destinations `session`, `localSettings`, `userSettings`, `projectSettings`.

Fixed reply strings (exact):
- `DENY_STOP = "The user doesn't want to proceed with this tool use. The tool use was rejected (eg. if it was a file edit, the new_string was NOT written to the file). STOP what you are doing and wait for the user to tell you how to proceed."` Confirm the full sentence with `grep -o '.\{300\}STOP what you are doing.\{80\}' webview/index.js` before hard-coding.
- `DENY_REASON_PREFIX = "The user doesn't want to proceed with this tool use. The tool use was rejected (eg. if it was a file edit, the new_string was NOT written to the file). The user provided the following reason for the rejection: "` followed by the user's text.
- `KEEP_PLANNING = "User chose to stay in plan mode and continue planning"`, `CANCEL_EDIT = "User cancelled the edit"`, `CANCEL_WRITE = "User cancelled the write"`, `REVERTED_ALL = "User reverted all proposed changes in the diff view"`.

Editor context blocks (first in the content array, user text last):
- `<ide_selection>The user selected the lines {start} to {end} from {abs_path}:\n{text}\n\nThis may or may not be related to the current task.</ide_selection>` (1-based inclusive).
- `<ide_opened_file>The user opened the file {abs_path} in the IDE. This may or may not be related to the current task.</ide_opened_file>`.

Mentions: `@rel/path`, `@rel/path#5`, `@rel/path#5-10` (1-based, `/` separators); a path with whitespace or `:` is quoted with the suffix inside (`@"my file.ts#5-10"`); a path containing `"` or `#` gets no line suffix.

IDE server: lock file `<CLAUDE_CONFIG_DIR or ~/.claude>/ide/<port>.lock` = `{"pid":…,"workspaceFolders":[…],"ideName":"Wu","transport":"ws","runningInWindows":bool,"authToken":"<uuid>"}`; WebSocket on `127.0.0.1:<random 10000-65535>`; header `x-claude-code-ide-authorization`; terminals get `CLAUDE_CODE_SSE_PORT=<port>`. Tools: `openDiff{old_file_path,new_file_path,new_file_contents,tab_name}` returning two text blocks (`FILE_SAVED` + final contents, or `DIFF_REJECTED` + tab name), `getDiagnostics{uri?}` returning `[{uri,linesInFile,diagnostics:[{message,severity,range{start{line,character},end{…}},source,code}]}]`, `close_tab{tab_name}` returning `TAB_CLOSED`, `closeAllDiffTabs{}` returning `CLOSED_<n>_DIFF_TABS`. Notifications: `selection_changed{text,filePath,fileUrl,selection{start,end,isEmpty}}` (0-based, 300 ms debounce), `at_mentioned{filePath,lineStart?,lineEnd?}` (0-based).

In-process MCP: CLI sends control request `{subtype:"mcp_message",server_name,message:<JSON-RPC>}`; reply `{mcp_response:<JSON-RPC reply>}`, or `{mcp_response:{jsonrpc:"2.0",result:{},id:0}}` for notifications.

Other frames: `{type:"prompt_suggestion",suggestion}`; system subtypes `session_title_changed{title}`, `task_progress{task_id,usage{total_tokens,tool_uses,duration_ms},last_tool_name}`, `task_updated{task_id,patch{status,is_backgrounded}}`.

## File Structure

| File | Responsibility | Tasks |
|---|---|---|
| `crates/agent_harness/src/proto.rs` | new shared types: `PermissionPrompt`, `PermissionDecision`, `PermissionMode::Plan`, new `AgentEvent`s | 3, 5, 12, 38, 39 |
| `crates/agent_harness/src/agent_harness.rs` | `Steer::Control`, `ControlCall`, `Harness::control_once` | 2 |
| `crates/agent_harness/src/claude/wire.rs` | line builders: `control_request_line`, `permission_response`, context blocks | 2, 3, 15 |
| `crates/agent_harness/src/claude.rs` | run loop wiring, spawn env, `hook_callback`, `mcp_message` | 2, 3, 11, 12, 21 |
| `crates/agent_harness/src/claude/normalize.rs` | new frames to events | 38, 39, 43 |
| `crates/agent_harness/tests/fixtures/fake-claude.sh` (+ new `fake-claude.cmd`) | scripted CLI scenarios | 1 and most harness tasks |
| `crates/agent_chat/src/session.rs` | `control()`, `decide_permission()`, checkpoints, archive, groups | 2, 3, 12-14, 33-37 |
| `crates/agent_chat/src/chat_view/composer.rs` | permission card, plan card, footer chips | 4, 6, 15, 38 |
| `crates/agent_chat/src/chat_view/edit_review.rs` (new) | proposed text, review decision, review tab | 8-10 |
| `crates/agent_chat/src/chat_view/editor_context.rs` (new) | selection tracking, context blocks, mention text | 15-19 |
| `crates/agent_chat/src/ide_server.rs` (new) | diagnostics JSON, in-process MCP, WebSocket IDE server | 21-23 |
| `crates/agent_chat/src/chat_view/claude_dialogs.rs` (new) | one list-dialog used by `/mcp`, `/permissions`, `/hooks`, `/memory`, `/skills`, `/status`, `/sandbox`, `/plugins`, `/usage` | 24-29 |
| `crates/agent_chat/src/chat_view/plugin_ui.rs` (new) | plugin drawing: attach, tree model, rendering, panes | 47-49 |
| `crates/agent_chat/src/slash_commands.rs`, `chat_view/completion.rs` | native commands | 7, 14, 24-32 |
| `crates/agent_chat/src/agent_panel.rs` | archive, groups, filters | 33-35 |
| `crates/editor/src/editor.rs` | `add_code_action_provider` | 18 |
| `crates/project_panel/src/project_panel.rs`, `crates/git_ui/src/git_panel.rs` | "Add to Chat", "Resolve Merge Conflicts" entries | 17, 19 |
| `crates/wu_actions/src/lib.rs` | cross-crate actions `agent::AddToChat`, `agent::InsertMention` | 16, 17 |
| `crates/wu/src/wu/open_listener.rs`, `crates/wu/src/main.rs` | `wu://agent/...` links | 41 |
| `crates/terminal/src/terminal.rs:660` | `CLAUDE_CODE_SSE_PORT` env | 23 |

---

# Phase 0: Foundations

### Task 1: Make the fake-CLI tests run on Windows

Today `crates/agent_harness/tests/claude.rs` and `tests/claude_models.rs` start with `#![cfg(unix)]`, so none of the scripted-CLI tests can verify anything on this machine. Every later harness task depends on this.

**Files:**
- Create: `crates/agent_harness/tests/fixtures/fake-claude.cmd`
- Modify: `crates/agent_harness/tests/claude.rs:1-33`, `crates/agent_harness/tests/claude_models.rs` (fixture helper), `crates/agent_chat/src/session.rs:4421` (`new_store`) and the three `#[cfg(unix)]` session tests (5015, 5080, 7213)

**Interfaces:**
- Produces: `fixture_path()` returns `fake-claude.cmd` on Windows and `fake-claude.sh` elsewhere. New scenarios are added as `*scenario:<name>*)` branches in `fake-claude.sh` only.

- [ ] **Step 1:** Create `fake-claude.cmd` with exactly two lines: `@echo off` and `bash "%~dp0fake-claude.sh" %*`. Git for Windows provides `bash` on PATH.
- [ ] **Step 2:** In `tests/claude.rs` remove `#![cfg(unix)]`; make `fixture_path()` pick the `.cmd` file under `#[cfg(windows)]`. In `claude_models.rs` write a `claude.cmd` next to the copied Python file containing `@python "%~dp0claude" %*` on Windows (the machine has `python`, not `python3`).
- [ ] **Step 3:** Run `cargo test -p agent_harness --test claude --test claude_models`. Expected: compiles and runs on Windows. Rule for failures: a test that fails only because of Unix signals, file modes or process-group kills gets its own `#[cfg(unix)]` with a one-line comment naming the reason; any other failure is fixed. Record the count of passing and gated tests in the commit message.
- [ ] **Step 4:** Point `new_store` in `session.rs` at the same per-platform fixture and remove the three `#[cfg(unix)]` gates where the tests now pass. Run `cargo test -p agent_chat session::tests`. Expected: PASS.
- [ ] **Step 5:** Commit `test(agent-harness): run the fake Claude CLI tests on Windows`.

### Task 2: Generic control channel (request with response)

**Files:**
- Modify: `crates/agent_harness/src/agent_harness.rs:57-121`, `crates/agent_harness/src/claude/wire.rs`, `crates/agent_harness/src/claude.rs` (run loop `steering.recv()` arm at ~1179, stdout arm at ~1040, constants at 1329), `crates/agent_harness/src/{codex,opencode,acp}.rs` (new `Steer` arm), `crates/agent_chat/src/session.rs`
- Test: inline in `wire.rs`, `crates/agent_harness/tests/claude.rs`, `fake-claude.sh` scenario `control`

**Interfaces:**
- Produces:
```rust
// agent_harness.rs
pub struct ControlCall { pub request: serde_json::Value, pub reply: tokio::sync::oneshot::Sender<Result<serde_json::Value, String>> }
pub enum Steer { Message(SteerMessage), StopTurn(oneshot::Sender<()>), Control(ControlCall) }
// trait Harness, default body returns Err(HarnessError::Protocol("This agent has no control channel.".into()))
async fn control_once(&self, cwd: &Path, resume: Option<&str>, request: serde_json::Value) -> Result<serde_json::Value, HarnessError>;
// wire.rs
pub(crate) fn control_request_line(request_id: &str, request: &Value) -> String;
pub(crate) fn control_reply(line: &str, request_id_prefix: &str) -> Option<(String, Result<Value, String>)>;
// claude.rs
const CONTROL_REQUEST_PREFIX: &str = "wu-ctl-";
// session.rs: uses the live run when there is one, else harness.control_once(cwd, native_session_id); times out after 30 s
pub fn control(&mut self, request: serde_json::Value, cx: &mut Context<Self>) -> Task<anyhow::Result<serde_json::Value>>;
```

- [ ] **Step 1: Tests.** `wire.rs`: `control_request_line_shape` (`v["type"]=="control_request"`, `v["request_id"]=="wu-ctl-1"`, `v["request"]["subtype"]=="get_plan"`); `control_reply_matches_prefix_only` (success yields `Ok(json!({"exists":true}))`, error yields `Err("nope")`, a reply to `wu-set-mode-1` yields `None`). `tests/claude.rs`: `control_call_round_trips` (scenario `control`: send `Steer::Control` with `{"subtype":"get_plan"}` after the first turn, assert the reply is `Ok` with `["exists"] == true`) and `control_error_and_timeout_surface` (scenario replies with `subtype:error` for `mcp_status`, assert `Err` containing the CLI's text; a `ControlCall` whose run ends before an answer resolves with `Err`, not a hang). `session.rs`: `control_without_a_run_uses_control_once` using the fake CLI.
- [ ] **Step 2: Implement.** Run loop keeps `pending_controls: HashMap<String, oneshot::Sender<Result<Value, String>>>`; on `Steer::Control` write the line and store the sender; in the stdout arm resolve via `wire::control_reply` before normal frame handling; on loop exit drop the map so receivers get an error. `ClaudeHarness::control_once` spawns with `build_command` flags plus `--no-session-persistence` when `resume` is `None`, sends `initialize` then the request, returns the matched response, closes stdin. Other harnesses reply `Err("unsupported")` to `Steer::Control`.
- [ ] **Step 3: Verify.** `cargo test -p agent_harness control` and `cargo test -p agent_chat control_without_a_run`. Expected: PASS.
- [ ] **Step 4:** Commit `feat(agent-harness): add a request/response control channel`.

### Task 3: Typed permission prompt and decision

**Files:**
- Modify: `crates/agent_harness/src/proto.rs:362-384`, `crates/agent_harness/src/claude/wire.rs:364-370`, `crates/agent_harness/src/claude.rs:39-66` and `1368-1431`, every `UserInputQuestion {` / `UserInputAnswer {` literal in the workspace (grep both), `crates/agent_chat/src/session.rs:978-1003` and `1790-1860`

**Interfaces:**
- Produces:
```rust
// proto.rs (all serde camelCase, all new fields #[serde(default)])
pub struct PermissionPrompt { pub tool_name: String, pub input: serde_json::Value, pub tool_use_id: Option<String>, pub suggestions: Vec<serde_json::Value>, pub allows_always: bool, pub blocked_path: Option<String>, pub title: Option<String>, pub description: Option<String> }
pub enum PermissionDecision { Allow { updated_input: Option<serde_json::Value>, updated_permissions: Vec<serde_json::Value> }, Deny { message: String, interrupt: bool } }
// UserInputQuestion gains: pub permission: Option<PermissionPrompt>
// UserInputAnswer gains:   pub decision: Option<PermissionDecision>
// wire.rs
pub(crate) fn permission_response(decision: &PermissionDecision, original_input: &Value, tool_use_id: Option<&str>) -> Value;
// claude.rs
pub const DENY_STOP: &str; pub const DENY_REASON_PREFIX: &str;
// session.rs
pub fn decide_permission(&mut self, id: &str, decision: PermissionDecision, cx: &mut Context<Self>);
```
- `allows_always` is true when `permission_suggestions` is non-empty, `suppress_always_allow_rule` is not set and the request has no `plugin`.
- `answer_permission(id, allow, cx)` stays and maps to `decide_permission` with `Allow{None, vec![]}` or `Deny{DENY_STOP, interrupt: true}`. A label-only answer (other harnesses) keeps working.

- [ ] **Step 1: Tests.** `wire.rs`: `allow_reply_always_carries_input_and_tool_use_id` (`Allow{None,[]}` with original `{"command":"ls"}` and id `t1` yields `{"behavior":"allow","updatedInput":{"command":"ls"},"updatedPermissions":[],"toolUseID":"t1"}`); `deny_reply_carries_interrupt` (`{"behavior":"deny","message":"m","interrupt":true,"toolUseID":"t1"}`). `claude.rs` unit test via `answer_to`: `permission_prompt_carries_suggestions` (a `can_use_tool` with one `addRules` suggestion yields `question.permission.suggestions.len()==1` and `allows_always`); `dropped_permission_prompt_denies` (drop the sender; the written reply has `behavior == "deny"`). `session.rs`: `decide_permission_resolves_the_pending_question`.
- [ ] **Step 2: Implement.** Extend `ControlRequestBody` reads through `other`; build `PermissionPrompt` in `permission_question`; in `handle_control_request` prefer `answer.decision`, fall back to the Allow/Deny label.
- [ ] **Step 3: Verify.** `cargo test -p agent_harness permission` and `cargo test -p agent_chat permission`; then `cargo build -p wu` to prove every struct literal was updated.
- [ ] **Step 4:** Commit `feat(agent-harness): carry typed permission prompts and decisions`.

---

# Phase 1: Permissions and plan mode

### Task 4: "Always allow", deny with a reason, editable Bash command

**Files:**
- Modify: `crates/agent_chat/src/chat_view/composer.rs:734-851` (`render_question_panel`), `crates/agent_chat/src/session.rs:2702` (`ChatListPrefs` gains `permission_destination: Option<String>`)

**Interfaces:**
- Consumes: `PermissionPrompt`, `PermissionDecision`, `decide_permission` (Task 3).
- Produces: `pub(crate) fn always_allow_updates(suggestions: &[Value], destination: &str) -> Vec<Value>` in `composer.rs`.

- [ ] **Step 1: Tests.** `always_allow_overrides_destination_and_drops_set_mode`: input `[addRules(dest session), setMode(dest session)]` with destination `localSettings` yields one entry, `type == "addRules"`, `destination == "localSettings"`. `set_mode_only_keeps_its_own_destination`. `deny_with_text_uses_reason_template`: decision message equals `DENY_REASON_PREFIX + "use pnpm"` and `interrupt == false`. `edited_command_is_sent_as_updated_input`: Bash input `{"command":"ls","description":"d"}` edited to `ls -la` yields `updated_input == {"command":"ls -la","description":"d"}`.
- [ ] **Step 2: Implement.** Card rows: Allow, "Always allow for {destination label}" (only when `allows_always`; Left/Right arrow or a small menu cycles `localSettings` "this project (just you)", `userSettings` "all projects", `projectSettings` "this project (shared)", `session` "this session"; default = last choice from `ChatListPrefs`, else the suggestion's own, else `session`), Deny, and a one-line "Tell Claude what to do instead" field whose Enter denies with the reason template. For `Bash`, the command shown in the card is an editable single-line editor.
- [ ] **Step 3: Manual check.** In the dev app set the mode to Ask, ask Claude to run `git status`, pick "Always allow for this project (just you)". Expected: `.claude/settings.local.json` in the project gains the rule and the second `git status` is not asked.
- [ ] **Step 4:** Commit `feat(agent-chat): add always-allow, deny reasons and command editing to permission prompts`.

### Task 5: Plan as a permission mode

**Files:**
- Modify: `crates/agent_harness/src/proto.rs:36-83`, `crates/agent_harness/src/claude.rs:1341-1365`, `crates/agent_harness/src/view.rs:266-300` (`permission_label`, `permission_description`), settings `default_permission.claude_code` enum (`settings_content.rs:1298-1307`), zh-CN strings

**Interfaces:**
- Produces: `PermissionMode::Plan` (serde `plan`), offered only in `choices(HarnessId::ClaudeCode)`, placed after `Ask`; `permission_mode_name(Plan) == "plan"`; `permission_args(Plan) == ["--permission-mode", "plan"]`. `AgentEvent::PermissionModeReported{mode:"plan"}` maps back to `Plan` in the session.

- [ ] **Step 1: Tests.** `proto.rs`: `plan_is_claude_only` (`Plan.for_harness(Codex) == Auto`, `choices(ClaudeCode).contains(&Plan)`). `claude.rs`: `plan_mode_uses_the_plan_flag`. `tests/claude.rs`: extend scenario `mode-change` to assert a live switch to `Plan` writes `"mode":"plan"`.
- [ ] **Step 2: Implement**, including label "Plan" and description "Claude explores and writes a plan; nothing is changed until you approve it."
- [ ] **Step 3: Verify.** `cargo test -p agent_harness plan` and `cargo test -p settings --lib` (schema translations).
- [ ] **Step 4:** Commit `feat(agent-harness): add plan permission mode for Claude`.

### Task 6: Plan approval card (`ExitPlanMode`)

**Files:**
- Modify: `crates/agent_chat/src/chat_view/composer.rs` (new `render_plan_panel`), `crates/agent_chat/src/session.rs` (`PendingQuestion::is_plan()`), `crates/agent_harness/src/claude.rs:39-66` (question text "Ready to code?")

**Interfaces:**
- Produces:
```rust
pub(crate) enum PlanChoice { AcceptEdits, Auto, Manual, KeepPlanning { feedback: String, comments: Vec<PlanComment> } }
pub(crate) struct PlanComment { pub quote: String, pub text: String }
pub(crate) fn plan_decision(choice: &PlanChoice) -> (Option<PermissionMode>, PermissionDecision);
```
- Mapping: `AcceptEdits` and `Auto` return `(Some(mode), Allow{None, []})` and the session applies the mode with the existing live `set_permission_mode` **before** answering; `Manual` returns `(None, Allow{None, [{"type":"setMode","mode":"default","destination":"session"}]})` and the session's stored mode becomes `Ask`; `KeepPlanning` with no feedback and no comments returns `Deny{KEEP_PLANNING, interrupt: true}`; with feedback returns `Deny{DENY_REASON_PREFIX + feedback, interrupt: false}`; comments append `"\n\nComments on the plan:\n[Re: \"<quote>\"] <text>"`, one line per comment.

- [ ] **Step 1: Tests.** One test per mapping row above with the exact strings, plus `session.rs` `approving_a_plan_switches_the_session_mode` (after `AcceptEdits` the session's `settings.permission == AcceptEdits`).
- [ ] **Step 2: Implement.** The card renders `permission.input["plan"]` through the existing `Markdown` entity, scrollable, with buttons "Yes, and auto-accept edits" (or "Yes, and use auto mode" when the session's previous mode was Auto), "Yes, and manually approve edits", "No, keep planning", and a feedback field. Approve buttons are hidden while comments exist.
- [ ] **Step 3: Manual check.** Switch to Plan, ask "add a `--version` flag to script/clippy". Expected: no file changes, the plan card appears; "keep planning" with text "also update the help text" makes Claude revise; "Yes, and auto-accept edits" flips the mode row to Accept Edits and Claude edits.
- [ ] **Step 4:** Commit `feat(agent-chat): review and approve Claude's plans`.

### Task 7: Plan comments, "Open plan", `/plan`

**Files:**
- Modify: `crates/agent_chat/src/chat_view/composer.rs`, `crates/agent_chat/src/slash_commands.rs:82-141`, `crates/agent_chat/src/chat_view/completion.rs:625-692`, `crates/agent_harness/src/claude.rs:318-383` (remove `plan` from `HIDDEN_COMMANDS` if present)

**Interfaces:**
- Consumes: `control()` (Task 2) with `{"subtype":"get_plan"}`.
- Produces: `AppCommand::Plan { task: Option<String>, open: bool }`.

- [ ] **Step 1: Tests.** `slash_commands.rs`: `/plan` parses to `Plan{None,false}`, `/plan open` to `Plan{None,true}`, `/plan fix the build` to `Plan{Some("fix the build"),false}`. `completion.rs`: `plan_command_switches_mode_and_sends_task` (mode becomes `Plan`, the task text is sent as the prompt).
- [ ] **Step 2: Implement.** `/plan open` calls `get_plan` and opens `path` in the workspace when `exists`, else a toast "No plan yet". The plan card gets an "Open plan" button doing the same. Comments: selecting text in the plan and pressing "Comment" (check `crates/markdown` for a public selected-text accessor first; if there is none, add `pub fn selected_text(&self) -> Option<String>` to `Markdown`) adds a `PlanComment`; comments are listed under the plan with a remove button.
- [ ] **Step 3: Manual check.** In plan mode, comment on one sentence of a plan and press "No, keep planning". Expected: Claude's next message addresses the quoted sentence.
- [ ] **Step 4:** Commit `feat(agent-chat): add plan comments and the /plan command`.

---

# Phase 2: Reviewing edits

### Task 8: Proposed text and review decision (pure logic)

**Files:**
- Create: `crates/agent_chat/src/chat_view/edit_review.rs`
- Modify: `crates/agent_chat/src/chat_view.rs` (`mod edit_review;`)

**Interfaces:**
- Produces:
```rust
pub(crate) fn proposed_text(tool_name: &str, input: &serde_json::Value, original: &str) -> anyhow::Result<String>;
pub(crate) fn review_decision(tool_name: &str, input: &serde_json::Value, original: &str, proposed: &str, final_text: &str, disk_text: &str) -> PermissionDecision;
```
- `proposed_text`: `Write` returns `input["content"]`; `Edit` replaces the first `old_string` (all when `replace_all`), error when `old_string` is absent from `original`.
- `review_decision`: `disk_text != original` returns `Deny{"The file changed on disk while the edit was being reviewed. Read it again before editing.", interrupt: false}`; `final_text == original` returns `Deny{REVERTED_ALL, interrupt: true}`; `final_text == proposed` returns `Allow{None, []}`; otherwise `Edit` returns `Allow{Some({"file_path","old_string": original,"new_string": final_text,"replace_all": false}), []}` and `Write` returns `Allow{Some({"file_path","content": final_text}), []}`.

- [ ] **Step 1: Tests.** `edit_applies_first_match_only`, `edit_replace_all`, `edit_missing_old_string_errors`, `write_uses_content`, `unchanged_proposal_allows_original_input`, `user_edited_proposal_sends_whole_file_edit` (asserts the exact four keys), `reverting_everything_denies_with_reverted_message`, `stale_original_denies_instead_of_overwriting`.
- [ ] **Step 2: Implement** and run `cargo test -p agent_chat edit_review`. Expected: PASS.
- [ ] **Step 3:** Commit `feat(agent-chat): compute proposed edits and review decisions`.

### Task 9: Review tab with Accept and Reject

**Files:**
- Modify: `crates/agent_chat/src/chat_view/edit_review.rs`, `crates/agent_chat/src/chat_view/composer.rs` (permission card for `Edit`/`Write`), `crates/agent_chat/Cargo.toml` (dependency `git_ui_core`), `crates/agent_chat/src/agent_chat.rs:33-73` (actions), keymaps

**Interfaces:**
- Consumes: `git_ui_core::FileDiffView::new(old_buffer, new_buffer, diff, project, workspace, window, cx)` and `build_buffer_diff` (`crates/git_ui_core/src/file_diff_view.rs:105`, `224`); Task 8 functions.
- Produces: `pub(crate) fn open_review(session: Entity<AgentSession>, question_id: String, prompt: &PermissionPrompt, workspace: WeakEntity<Workspace>, window: &mut Window, cx: &mut App) -> Task<anyhow::Result<()>>`; actions `agent_chat::AcceptProposedChanges`, `agent_chat::RejectProposedChanges`.

- [ ] **Step 1:** Run `cargo tree -p git_ui_core -i agent_chat`. Expected: no path (no dependency cycle). If there is one, move `open_review` behind an `agent_chat::set_review_opener(fn)` hook registered from `git_ui`, the same way `git_ui_core::set_branch_picker_builder` works.
- [ ] **Step 2: Test.** `chat_panel.rs`-style `#[gpui::test]` `accepting_a_review_allows_the_edit`: `FakeFs` file `a.rs` = `"one\n"`, a pending `Edit` prompt `one` to `two`; `open_review`; dispatch `AcceptProposedChanges`; assert the session received `Allow{None, []}` and the review tab closed. `rejecting_a_review_denies_with_cancel_message` asserts `Deny{CANCEL_EDIT, interrupt: true}`. `closing_the_review_tab_rejects`.
- [ ] **Step 3: Implement.** Left buffer = file from disk (empty for a new file), right buffer = `proposed_text`, editable, titled "{file} (proposed)". Toolbar buttons Accept and Reject. The chat permission card for `Edit`/`Write` shows the existing inline diff plus "Review in editor". In Ask mode the review opens automatically. Answering in either place closes the other.
- [ ] **Step 4: Manual check.** Ask mode, "rename the function `foo` in <file>". Expected: a diff tab opens; edit one proposed line by hand, Accept; the file on disk contains your hand-edit.
- [ ] **Step 5:** Commit `feat(agent-chat): review proposed edits in a diff tab`.

### Task 10: Accept or reject individual changes

**Files:**
- Modify: `crates/agent_chat/src/chat_view/edit_review.rs`, `crates/agent_chat/src/agent_chat.rs` (actions), keymaps, editor right-click menu is not changed (the actions are reachable from the hunk buttons and the command palette)

**Interfaces:**
- Consumes: `editor::DiffHunkRenderer` and `Editor::set_diff_hunk_renderer` (`crates/editor/src/git.rs:25`, `249`), `SplittableEditor::set_diff_hunk_renderer` (`split.rs:586`).
- Produces: actions `agent_chat::AcceptChangeAtCursor`, `agent_chat::RejectChangeAtCursor`.

- [ ] **Step 1: Test.** `rejecting_one_of_two_hunks_sends_the_remaining_change`: proposal changes lines 1 and 5; reject the hunk at line 5, accept the hunk at line 1; the decision is `Allow{Some(edit)}` whose `new_string` contains the line-1 change and the original line 5. `more_than_100_hunks_shows_no_hunk_buttons`.
- [ ] **Step 2: Implement.** A `DiffHunkRenderer` drawing "Accept this change" and "Reject this change". Reject restores that hunk in the right buffer to the left side's text. Accept marks the hunk decided. When every hunk is decided the review finishes exactly like Accept in Task 9 (so `review_decision` does the rest).
- [ ] **Step 3: Manual check.** A two-place edit: reject one, accept the other. Expected: only the accepted place changed on disk.
- [ ] **Step 4:** Commit `feat(agent-chat): accept or reject single changes in an edit review`.

### Task 11: Save open buffers before Claude reads or edits them

**Files:**
- Modify: `crates/agent_harness/src/claude/wire.rs:390` (`initialize_request_line` gains hooks), `crates/agent_harness/src/claude.rs:1374-1384` (`hook_callback` arm), `crates/agent_harness/src/proto.rs` (`AgentEvent::FileAccess`), `crates/agent_chat/src/session.rs` (`apply_event`), settings

**Interfaces:**
- Produces: `initialize` carries `"hooks":{"PreToolUse":[{"matcher":"Edit|Write|MultiEdit|Read","hookCallbackIds":["wu-presave"]}]}`; `RunControls` gains `pub before_file_tool: Arc<dyn Fn(String) -> oneshot::Receiver<()> + Send + Sync>` (argument = absolute file path); the harness answers `hook_callback` with `{}` after the receiver resolves or after 2 s. Setting `autosave` (bool, default true).

- [ ] **Step 1: Probe.** `grep -o '.\{200\}hook_callback.\{400\}' ~/.vscode/extensions/anthropic.claude-code-2.1.331-win32-x64/extension.js | head -3` and confirm the success reply shape for a hook callback (expected: the hook output object, `{}` meaning continue). Write the confirmed shape in a comment above the arm.
- [ ] **Step 2: Tests.** `tests/claude.rs` scenario `hook`: the fake CLI sends `hook_callback` with `callback_id:"wu-presave"` and `input.tool_input.file_path:"/x/a.rs"`; assert `before_file_tool` was called with `/x/a.rs` and a success reply was written. `session.rs`: `dirty_buffer_is_saved_before_the_tool_runs` with `FakeFs`.
- [ ] **Step 3: Implement.** The session saves the open dirty buffer for that path through the project, then resolves. When `autosave` is false it resolves at once.
- [ ] **Step 4: Manual check.** Type in a file without saving, ask Claude "what is on line 1 of <file>". Expected: the answer reflects the unsaved text and the tab is no longer dirty.
- [ ] **Step 5:** Commit `feat(agent-chat): save dirty buffers before Claude reads or edits them`.

---

# Phase 3: Checkpoints

### Task 12: Turn on file checkpoints and remember each prompt's id

**Files:**
- Modify: `crates/agent_harness/src/claude.rs:157-224` (`build_command` env), `crates/agent_chat/src/session.rs:373` (`Entry::User`), `:390` (`SerializedEntry`), `apply_event` for `PromptRecorded`

**Interfaces:**
- Produces: the child gets env `CLAUDE_CODE_ENABLE_SDK_FILE_CHECKPOINTING=true`; `Entry::User` and its serialized form gain `prompt_id: Option<String>` (`#[serde(default)]`), filled from `AgentEvent::PromptRecorded { id }` in send order.

- [ ] **Step 1: Tests.** `claude.rs`: `build_command_enables_file_checkpointing` (inspect `cmd.as_std().get_envs()`). `session.rs`: `recorded_prompt_ids_attach_to_user_entries_in_order` (two user entries, two `PromptRecorded`, each entry gets its own id) and `old_transcripts_load_without_prompt_ids`.
- [ ] **Step 2: Implement**, then `cargo test -p agent_chat prompt_id` and `cargo test -p agent_harness checkpointing`.
- [ ] **Step 3: Probe with the real CLI.** In the dev app send one prompt that edits a file, then run `control({"subtype":"rewind_files","user_message_id":<that id>,"dry_run":true})` from a temporary debug action. Expected: `canRewind: true` and the edited file in `filesChanged`. If `canRewind` is false, the id wu records is not the checkpoint id: compare against the `uuid` of the `type:"user"` line in `~/.claude/projects/<project>/<session>.jsonl` and fix the mapping before continuing. Remove the debug action.
- [ ] **Step 4:** Commit `feat(agent-chat): enable Claude file checkpoints and track prompt ids`.

### Task 13: Rewind code to a message

**Files:**
- Modify: `crates/agent_chat/src/session.rs`, `crates/agent_chat/src/chat_view.rs` (user-message hover row at ~1628-1640)

**Interfaces:**
- Consumes: `control()`, `Entry::User.prompt_id`.
- Produces:
```rust
pub struct RewindPreview { pub can_rewind: bool, pub files_changed: Vec<String>, pub insertions: u64, pub deletions: u64, pub error: Option<String> }
pub fn rewind_preview(&mut self, entry_index: usize, cx: &mut Context<Self>) -> Task<anyhow::Result<RewindPreview>>;
pub fn rewind_code(&mut self, entry_index: usize, cx: &mut Context<Self>) -> Task<anyhow::Result<RewindPreview>>;
```
- `rewind_code` stops a running turn first (existing stop path), waits for idle, then sends the real request.

- [ ] **Step 1: Tests.** Fake scenario `rewind` answers `rewind_files` (`dry_run` true and false). `rewind_preview_parses_counts`; `rewind_refused_when_cannot_rewind` (`canRewind:false,error:"No checkpoint"` yields `can_rewind == false`, the message surfaces as a `Notice` entry, and no second request is sent); `rewind_on_entry_without_prompt_id_errors`.
- [ ] **Step 2: Implement.** Hovering a user message shows a rewind button opening a menu (Task 14 adds the other two entries). "Rewind code to here" shows a confirm prompt listing `files_changed` with `+insertions -deletions`, then calls `rewind_code` and adds a `Notice` "Rewound N files to this point."
- [ ] **Step 3: Manual check.** Two prompts that each edit a file; rewind code to the second prompt. Expected: the second prompt's edit is gone from disk, the first remains, the conversation is untouched.
- [ ] **Step 4:** Commit `feat(agent-chat): rewind code to an earlier message`.

### Task 14: Fork from any message, fork-and-rewind, `/fork`, `/rewind`

**Files:**
- Modify: `crates/agent_chat/src/session.rs:3983-4042`, `crates/agent_harness/src/claude.rs:687-709` and `:202-204`, `crates/agent_chat/src/chat_view.rs`, `crates/agent_chat/src/slash_commands.rs`, `crates/agent_harness/src/claude.rs:318-383` (un-hide `rewind`, `fork`)

**Interfaces:**
- Produces: `pub fn fork_session_at(&mut self, id: &str, entry_index: usize, cx: &mut Context<Self>) -> Task<anyhow::Result<Entity<AgentSession>>>` on `AgentStore`: the new chat's transcript is everything before `entry_index`, its `fork_source` is the parent's native id, its `dropped_prompt` is that entry's `prompt_id`, and its composer is prefilled with that entry's text (not sent). `AppCommand::Fork { text: Option<String> }`, `AppCommand::Rewind`.
- Harness: when a request has `fork` and `drop_prompt` the command line is `--resume=<fork id> --fork-session --resume-session-at=<resolved id>`. (The extension copies the JSONL by hand instead; the CLI flags give the same result with no new code.)

- [ ] **Step 1: Tests.** `claude.rs`: `fork_with_drop_prompt_resumes_at_the_earlier_message` (asserts the three flags). `session.rs`: `fork_at_truncates_and_prefills` (3 user entries, fork at index of the 2nd: new transcript has the first exchange only, composer draft equals the 2nd prompt's text); `fork_at_first_message_starts_empty_with_prefill`. `slash_commands.rs`: `/fork` and `/fork try again` parse.
- [ ] **Step 2: Implement.** Menu entries on a user message: "Fork conversation from here", "Rewind code to here", "Fork conversation and rewind code" (rewind first; fork only when the rewind succeeded). `/fork` forks from the latest message when idle; `/fork <text>` puts `<text>` in the new composer. `/rewind` opens the menu on the latest user message.
- [ ] **Step 3: Manual check.** Three-prompt chat; "Fork conversation from here" on prompt 2. Expected: a new tab titled "<title> (fork)" with prompt 1's exchange and prompt 2 waiting in the composer; asking "what did I ask before this?" in the fork mentions prompt 1 only.
- [ ] **Step 4:** Commit `feat(agent-chat): fork a chat from any message`.

---

# Phase 4: Editor context

### Task 15: Send the selection and the open file automatically

**Files:**
- Create: `crates/agent_chat/src/chat_view/editor_context.rs`
- Modify: `crates/agent_harness/src/proto.rs:123` (`RunRequest.context: Vec<String>`), `agent_harness.rs:57` (`SteerMessage.context`), `crates/agent_harness/src/claude/wire.rs:288` (`user_content(text, images, context)`), `crates/agent_chat/src/session.rs:309` (`Prompt.context`), `crates/agent_chat/src/chat_view/composer.rs:371-445` (footer chip), settings

**Interfaces:**
- Produces:
```rust
pub(crate) struct EditorContext { pub absolute_path: PathBuf, pub selection: Option<SelectedLines>, pub path_only: bool }
pub(crate) struct SelectedLines { pub start: u32, pub end: u32, pub text: String } // 1-based inclusive
pub(crate) fn current_editor_context(workspace: &Workspace, cx: &App) -> Option<EditorContext>;
pub(crate) fn context_block(context: &EditorContext) -> String;
```
- Settings `attach_selection` (default true), `attach_open_file` (default true). `path_only` is true for git-ignored or excluded files (worktree entry `is_ignored`) and forces the `<ide_opened_file>` form.

- [ ] **Step 1: Tests.** `selection_block_matches_the_ide_format` and `opened_file_block_matches_the_ide_format` compare against the two exact templates in the Protocol Reference. `wire.rs`: `context_blocks_come_first_and_text_last` (content array = `[context text, image, user text]`). `chat_view.rs`: `selection_is_sent_once_until_it_changes`, `slash_commands_get_no_context`, `ignored_file_sends_path_only`, `dismissed_context_is_not_sent`.
- [ ] **Step 2: Implement.** `ChatView` observes the workspace's active item and the active editor's selection (pattern: `workspace.active_item_as::<Editor>(cx)`, `Editor::copy_file_location` at `editor.rs:8753` for line ranges). Footer chip shows "{file}" or "{n} lines selected" with an eye toggle and an X; the last sent context is remembered so the same selection is not resent.
- [ ] **Step 3: Manual check.** Select 3 lines in an editor, type "what does this do?" with no file name. Expected: the chip reads "3 lines selected" and the answer is about those lines.
- [ ] **Step 4:** Commit `feat(agent-chat): attach the editor selection and open file to prompts`.

### Task 16: `@file#5-10` mentions and "Insert @-Mention Reference" (Alt+K)

**Files:**
- Modify: `crates/agent_chat/src/chat_view/editor_context.rs`, `crates/agent_chat/src/chat_view/completion.rs:103-110` and `:568`, `crates/wu_actions/src/lib.rs`, `crates/agent_chat/src/agent_chat.rs` (workspace action handler), keymaps (context `Editor`: `alt-k`, macOS also `alt-k`)

**Interfaces:**
- Produces: `pub(crate) fn mention_text_with_lines(relative_path: &str, lines: Option<(u32, u32)>) -> String`; action `wu_actions::agent::InsertMention`; `ChatView::insert_text_at_cursor(&mut self, text: &str, window, cx)`.

- [ ] **Step 1: Tests.** Table test `mention_formats`: `("src/a.rs", Some((5,10)))` gives `@src/a.rs#5-10`; `Some((5,5))` gives `@src/a.rs#5`; `None` gives `@src/a.rs`; `"my file.ts"` with `(5,10)` gives `@"my file.ts#5-10"`; `"C:/x/a.rs"` with `None` gives `@"C:/x/a.rs"`; `"a#b.rs"` with `(1,2)` gives `@a#b.rs` (no suffix). `agent_chat.rs`: `insert_mention_targets_the_last_focused_chat_and_opens_the_panel_when_none`.
- [ ] **Step 2: Implement**; the completion menu keeps accepting `#5-10` typed after a picked file (the badge fold must include the suffix).
- [ ] **Step 3: Manual check.** Select lines 5-10, press Alt+K. Expected: the chat composer gains `@path#5-10 ` and focus; sending "explain" answers about those lines.
- [ ] **Step 4:** Commit `feat(agent-chat): mention files with line ranges`.

### Task 17: "Add to Chat" in the project panel and the git panel

**Files:**
- Modify: `crates/wu_actions/src/lib.rs` (`agent::AddToChat { paths: Vec<PathBuf> }`), `crates/project_panel/src/project_panel.rs:1212-1343`, `crates/git_ui/src/git_panel.rs` (entry context menu), `crates/agent_chat/src/agent_chat.rs`

**Interfaces:**
- Consumes: `ChatView::insert_mentions(paths, window, cx)` (`completion.rs:568`).

- [ ] **Step 1: Test.** `agent_chat.rs`: `add_to_chat_inserts_mentions_for_every_path` (two files and one folder yield `@a.rs @b.rs @dir/ `). `project_panel` test: the context menu for a file contains "Add to Chat" when `agent_chat.enabled`.
- [ ] **Step 2: Implement** (multi-selection in the project panel passes every marked entry).
- [ ] **Step 3: Manual check.** Right-click two files, "Add to Chat". Expected: two mentions in the composer.
- [ ] **Step 4:** Commit `feat(agent-chat): add files to the chat from the project and git panels`.

### Task 18: "Fix with Claude" quick fix on diagnostics

**Files:**
- Modify: `crates/editor/src/editor.rs:1045` and `:2257` (new `pub fn add_code_action_provider(&mut self, provider: Rc<dyn CodeActionProvider>, window: &mut Window, cx: &mut Context<Self>)`, ignoring a provider whose `id()` is already present), `crates/agent_chat/src/chat_view/editor_context.rs` (`struct FixWithClaude`), `crates/agent_chat/src/agent_chat.rs:init` (`cx.observe_new::<Editor>`), settings

**Interfaces:**
- Produces: setting `show_fix_actions` (default true); `pub(crate) fn fix_prompt(relative_path: &str, diagnostics: &[(u32, u32, String, String)]) -> (String, String)` returning (composer text, attachment text): one diagnostic gives `Fix this problem in @rel/path#L<a>-<b> `, several give `Fix these problems in @rel/path#L<a>-<b> ` (range = union, 1-based); the attachment lists each as `{severity}: {message} (line {n})`.
- The action is an `LspAction::Action` with title "Fix with Claude" and `server_id: LanguageServerId(0)`, offered only when the range has a diagnostic of severity warning or error; applying it prefills the last-focused chat composer and adds the attachment as a context block (Task 15's `Prompt.context`). It never sends.

- [ ] **Step 1: Tests.** `fix_prompt_single_and_plural`; editor test `added_provider_is_not_duplicated`; `agent_chat` gpui test `fix_action_is_offered_only_on_diagnostics` (buffer with one error via `LspStore::update_diagnostic_entries`, `project.rs` / `lsp_store.rs:9549`) and `fix_action_prefills_without_sending`.
- [ ] **Step 2: Implement.**
- [ ] **Step 3: Manual check.** Introduce a type error in a Rust file, open the lightbulb on it. Expected: "Fix with Claude" is listed; choosing it fills the composer with `Fix this problem in @…#L…` and nothing is sent until Enter.
- [ ] **Step 4:** Commit `feat(agent-chat): offer Fix with Claude on diagnostics`.

### Task 19: "Resolve Merge Conflicts"

**Files:**
- Modify: `crates/git_ui/src/git_panel.rs` (button on the conflicts section header and entry context menu), `crates/wu_actions/src/lib.rs` (`agent::ResolveMergeConflicts { paths }`), `crates/agent_chat/src/agent_chat.rs`

- [ ] **Step 1: Test.** `resolve_conflicts_prefills_mentions`: paths `a.ts`, `dir/b.ts` give composer text `Resolve the merge conflicts in @a.ts @dir/b.ts ` and nothing is sent.
- [ ] **Step 2: Implement.**
- [ ] **Step 3: Manual check.** Create a conflicting merge in a scratch repo, press the button. Expected: the prompt lists exactly the conflicted files.
- [ ] **Step 4:** Commit `feat(agent-chat): resolve merge conflicts with Claude`.

### Task 20: Terminal references and document attachments

**Files:**
- Modify: `crates/agent_chat/src/chat_view/completion.rs` (mention source), `crates/agent_chat/src/chat_view/attachments.rs:217-300`, `crates/agent_harness/src/claude.rs:804-876` (`load_image_blocks` becomes `load_attachment_blocks`), `crates/agent_harness/src/claude/wire.rs:283`

**Interfaces:**
- Produces: mention `@terminal:<title>` in the completion list for every open terminal; on send its visible buffer text becomes a context block. Attachments: `.pdf` becomes `{"type":"document","source":{"type":"base64","media_type":"application/pdf","data":…},"title":<file name>}`, 32 MB limit.

- [ ] **Step 1: Probe.** `grep -o '.\{200\}<terminal name=.\{300\}' ~/.vscode/extensions/anthropic.claude-code-2.1.331-win32-x64/webview/index.js | head -2` and copy the exact wrapper into a constant `TERMINAL_BLOCK_TEMPLATE`.
- [ ] **Step 2: Tests.** `terminal_mention_becomes_a_context_block` (asserts the template with name and text filled); `pdf_attachment_is_a_document_block`; `oversized_pdf_reports_a_problem_and_is_skipped`.
- [ ] **Step 3: Implement**; manual check: run `cargo --version` in a terminal titled "build", send "what version is in @terminal:build?". Expected: the version is quoted.
- [ ] **Step 4:** Commit `feat(agent-chat): reference terminal output and attach PDFs`.

---

# Phase 5: IDE tools for the agent

### Task 21: Diagnostics tool for panel chats (in-process MCP)

**Files:**
- Create: `crates/agent_chat/src/ide_server.rs`
- Modify: `crates/agent_harness/src/claude/wire.rs:390` (`initialize` gains `"sdkMcpServers":["ide"]`), `crates/agent_harness/src/claude.rs:1374` (`mcp_message` arm), `agent_harness.rs:97` (`RunControls.mcp: Arc<dyn Fn(serde_json::Value) -> oneshot::Receiver<serde_json::Value> + Send + Sync>`), `crates/agent_chat/src/session.rs` (wire it)

**Interfaces:**
- Produces:
```rust
// ide_server.rs
pub(crate) fn diagnostics_json(project: &Entity<Project>, uri: Option<&str>, minimum_severity: Option<&str>, cx: &App) -> serde_json::Value; // the getDiagnostics array
pub(crate) fn handle_mcp_message(message: &serde_json::Value, tools: &dyn IdeTools) -> serde_json::Value; // JSON-RPC: initialize, tools/list, tools/call, notifications
pub(crate) trait IdeTools { fn call(&self, name: &str, arguments: &serde_json::Value) -> anyhow::Result<Vec<String>>; fn list(&self) -> serde_json::Value; }
```
- Panel tool list: `getDiagnostics{uri?, minSeverity?}` only. The model sees it as `mcp__ide__getDiagnostics`.

- [ ] **Step 1: Tests.** `handle_mcp_message_initialize_lists_and_calls` (initialize returns `protocolVersion` and `capabilities.tools`; `tools/list` has one tool named `getDiagnostics`; `tools/call` returns `content[0].type == "text"`; a notification returns `{"jsonrpc":"2.0","result":{},"id":0}`; an unknown tool returns a JSON-RPC error, not a panic). `diagnostics_json_shape` with a `FakeFs` project and one injected error: `[0].diagnostics[0].severity == "Error"`, 0-based `range`, `linesInFile` present. Harness scenario `mcp`: the fake CLI sends `mcp_message` and the reply is `{"mcp_response":{…}}`.
- [ ] **Step 2: Implement.**
- [ ] **Step 3: Manual check.** With a compile error in an open project ask "what errors does the editor report? use the ide diagnostics tool". Expected: a tool call `ide / getDiagnostics` whose result names the error.
- [ ] **Step 4:** Commit `feat(agent-chat): expose editor diagnostics to Claude`.

### Task 22: WebSocket IDE server and lock file (for `claude` in the terminal)

**Files:**
- Modify: `crates/agent_chat/src/ide_server.rs`, `crates/agent_chat/Cargo.toml` (`async-tungstenite` with the runtime feature the workspace already enables; check `Cargo.toml:400`), `crates/agent_chat/src/agent_chat.rs:init`

**Interfaces:**
- Produces:
```rust
pub struct IdeServer { pub port: u16, pub auth_token: String, /* lock path, task */ }
impl IdeServer { pub fn start(workspace_folders: Vec<PathBuf>, tools: Arc<dyn IdeTools + Send + Sync>, cx: &mut App) -> anyhow::Result<Self>; pub fn notify(&self, method: &str, params: serde_json::Value); }
pub(crate) fn lock_file_json(pid: u32, folders: &[PathBuf], auth_token: &str) -> serde_json::Value;
pub(crate) fn remove_stale_lock_files(directory: &Path, is_alive: impl Fn(u32) -> bool) -> std::io::Result<usize>;
```
- One server per wu process; `workspaceFolders` is rewritten when worktrees change; the lock file is deleted on drop. `pid` is wu's own process id (the terminal's `claude` is its descendant). Lock directory is created with owner-only permissions on Unix.

- [ ] **Step 1: Tests.** `lock_file_has_the_expected_fields` (`transport == "ws"`, `ideName == "Wu"`, `runningInWindows == cfg!(windows)`); `removes_stale_lock_files` (two lock files, one with a dead pid: 1 removed, the live one kept, a non-JSON file left alone); `rejects_wrong_token` (connect with a tungstenite client without the header and with a wrong one: both closed with code 1008); `tools_call_over_websocket` (correct header, `tools/list` returns the tools); `second_connection_replaces_the_first`.
- [ ] **Step 2: Implement** using `handle_mcp_message` from Task 21.
- [ ] **Step 3: Verify.** `cargo test -p agent_chat ide_server`. Expected: PASS.
- [ ] **Step 4:** Commit `feat(agent-chat): serve IDE tools to terminal Claude sessions`.

### Task 23: `openDiff`, selection notifications, terminal environment

**Files:**
- Modify: `crates/agent_chat/src/ide_server.rs`, `crates/agent_chat/src/chat_view/edit_review.rs` (reuse the review tab), `crates/terminal/src/terminal.rs:660`, `crates/agent_chat/src/chat_view/editor_context.rs`

**Interfaces:**
- Produces: terminal-server tools `openDiff`, `getDiagnostics`, `close_tab`, `closeAllDiffTabs` with the result strings in the Protocol Reference; `selection_changed` (300 ms debounce, resent 500 ms after a client connects) and `at_mentioned` (sent by `InsertMention` when the focused item is a terminal running `claude`, instead of touching the chat composer); a global `terminal::set_extra_env(fn() -> Vec<(String, String)>)`-style hook so `agent_chat` can add `CLAUDE_CODE_SSE_PORT` without `terminal` depending on it.
- `executeCode` is not offered: wu has no notebook support (see Deferred).

- [ ] **Step 1: Tests.** `open_diff_accept_returns_file_saved_and_contents` (two text blocks: `FILE_SAVED`, final text); `open_diff_reject_and_tab_close_return_diff_rejected` (`DIFF_REJECTED`, tab name); `close_all_diff_tabs_reports_count` (`CLOSED_2_DIFF_TABS`); `selection_changed_is_debounced_and_zero_based`; terminal test `extra_env_is_applied_to_new_terminals`.
- [ ] **Step 2: Implement.**
- [ ] **Step 3: Manual check.** Open a terminal in wu, run `echo $CLAUDE_CODE_SSE_PORT` (a port prints), run `claude`, then `/ide`. Expected: it reports being connected to Wu; ask it to edit a file in default mode and a wu diff tab opens; accepting it writes the file; selecting text in an editor shows "N lines selected" in the CLI's footer.
- [ ] **Step 4:** Commit `feat(agent-chat): connect terminal Claude sessions to the editor`.

---

# Phase 6: Management dialogs and commands

### Task 24: Dialog scaffold; `/status`, `/hooks`, `/memory`, `/skills`, `/sandbox`, output styles

**Files:**
- Create: `crates/agent_chat/src/chat_view/claude_dialogs.rs`
- Modify: `crates/agent_chat/src/slash_commands.rs`, `crates/agent_chat/src/chat_view/completion.rs:653-692`, `crates/agent_harness/src/claude.rs:318-383`

**Interfaces:**
- Produces:
```rust
pub(crate) struct DialogSection { pub title: SharedString, pub rows: Vec<DialogRow> }
pub(crate) struct DialogRow { pub label: SharedString, pub detail: Option<SharedString>, pub badge: Option<SharedString>, pub actions: Vec<RowAction> }
pub(crate) enum RowAction { OpenPath(PathBuf), OpenUrl(String), Control { label: SharedString, request: serde_json::Value }, Toggle { on: bool, request_on: serde_json::Value, request_off: serde_json::Value }, Shell { label: SharedString, arguments: Vec<String> } }
pub(crate) struct ClaudeDialog; // modal: title, sections, reload closure; runs RowAction::Control through AgentSession::control and reloads
pub(crate) fn status_sections(response: &Value) -> Vec<DialogSection>;
pub(crate) fn hooks_sections(response: &Value) -> Vec<DialogSection>;
pub(crate) fn memory_sections(response: &Value) -> Vec<DialogSection>;
pub(crate) fn skills_sections(response: &Value) -> Vec<DialogSection>;
pub(crate) fn sandbox_sections(response: &Value) -> Vec<DialogSection>;
```
- `AppCommand` gains `Status`, `Hooks`, `Memory`, `Skills`, `Sandbox`, `OutputStyle`; each is removed from `HIDDEN_COMMANDS`.
- A control error shows the CLI's message in the dialog body with a "Close" button.

- [ ] **Step 1: Tests.** One mapper test per function with a fixture response copied from the Protocol Reference shapes: e.g. `memory_sections_list_files_with_open_actions` (a `files[]` entry with `exists:true` yields a row whose action is `OpenPath(path)`; `auto_memory.toggleable` yields a `Toggle`), `hooks_sections_group_by_event`, `status_sections_keep_order`. `claude_dialogs.rs` gpui test: `control_error_is_shown_not_hung`.
- [ ] **Step 2: Probe** the auto-memory setting key: `python -I -c "import json,sys;s=json.load(open(sys.argv[1],encoding='utf-8'));print([k for k in s['properties'] if 'emory' in k])" ~/.vscode/extensions/anthropic.claude-code-2.1.331-win32-x64/claude-code-settings.schema.json` and use that key in the `Toggle`'s `update_settings{source:"userSettings",settings:{<key>: bool}}`.
- [ ] **Step 3: Implement.** Output styles: a picker over `initialize.available_output_styles` applying `apply_flag_settings{settings:{outputStyle}}`. "Instructions" row in `/memory` opens the project `CLAUDE.md` (created empty on first open after a confirm).
- [ ] **Step 4: Manual check.** `/memory` lists the user and project memory files and opens one in an editor tab; `/hooks` lists the hooks from `~/.claude/settings.json`; `/status` shows version and account.
- [ ] **Step 5:** Commit `feat(agent-chat): add status, hooks, memory, skills and sandbox dialogs`.

### Task 25: `/mcp`

**Files:**
- Modify: `crates/agent_chat/src/chat_view/claude_dialogs.rs`, `slash_commands.rs`, `HIDDEN_COMMANDS`

**Interfaces:**
- Produces: `pub(crate) fn mcp_sections(response: &Value) -> Vec<DialogSection>`; `AppCommand::Mcp`. Row actions by status: `connected` gives Disable (`mcp_toggle{enabled:false}`) and Reconnect; `disabled` gives Enable; `failed` gives Reconnect and shows `error`; `needs-auth` gives Authenticate (`mcp_authenticate`, then `OpenUrl(authUrl)`) ; any authenticated HTTP server gives "Clear authentication" (`mcp_clear_auth`). Footer: "Add server" (form: name, transport `http`/`sse`/`stdio`, URL or command, scope `local`/`user`/`project`) running `claude mcp add --transport <t> --scope <s> <name> <commandOrUrl>` through `RowAction::Shell`, and per-row "Remove" running `claude mcp remove --scope <s> <name>`. After a shell action, reload the list.

- [ ] **Step 1: Tests.** `mcp_rows_offer_actions_by_status` (one server per status; assert the action labels above); `add_server_builds_the_cli_arguments` (`["mcp","add","--transport","http","--scope","user","github","https://example.com/mcp"]`); `server_names_are_passed_as_single_arguments` (a name with spaces or `;` stays one argv entry, no shell string).
- [ ] **Step 2: Implement** (spawn with the harness's resolved executable and argument vector, never through a shell).
- [ ] **Step 3: Manual check.** `/mcp`, disable a connected server, ask Claude to use it (it cannot), enable it again.
- [ ] **Step 4:** Commit `feat(agent-chat): manage MCP servers from the chat`.

### Task 26: `/permissions`

**Files:**
- Modify: `claude_dialogs.rs`, `slash_commands.rs`, `HIDDEN_COMMANDS`

**Interfaces:**
- Produces: `pub(crate) fn permission_sections(response: &Value) -> Vec<DialogSection>` (sections Allow, Ask, Deny, Workspace directories; row label = `rule`, detail = `description.prefix + emphasis + suffix`, badge = `source`); `pub(crate) fn settings_with_rule(current: &Value, behavior: &str, rule: &str, add: bool) -> Value` producing the `permissions.{allow|ask|deny}` array to send via `update_settings`. Rows with `editability` that permits it get "Remove"; the footer has "Add rule" (behavior, rule text, source `userSettings` or `localSettings`). Project-shared and managed rules get `OpenPath` to their file instead.

- [ ] **Step 1: Tests.** `permission_rows_are_grouped_by_behavior`; `adding_a_rule_keeps_existing_rules_and_does_not_duplicate`; `removing_a_rule_leaves_the_others`; `managed_rules_have_no_remove_action`.
- [ ] **Step 2: Implement** (read current settings with `get_settings`, pick the matching `sources[]` entry, send the merged object).
- [ ] **Step 3: Manual check.** Add `Bash(git status)` to Allow for "just you", confirm it appears in `.claude/settings.local.json` and in the dialog after reopening; remove it.
- [ ] **Step 4:** Commit `feat(agent-chat): view and edit permission rules`.

### Task 27: `/plugins`

**Files:**
- Modify: `claude_dialogs.rs`, `slash_commands.rs`, `HIDDEN_COMMANDS`

**Interfaces:**
- Produces: `AppCommand::Plugins`; two tabs. Plugins: rows from `claude plugin list --json`, toggle running `claude plugin enable|disable <id>`, trash running `claude plugin uninstall <id>`, "Install" (id as `plugin@marketplace`, scope) running `claude plugin install <id> --scope <user|project|local>`. Marketplaces: rows from `claude plugin marketplace list --json`, add (`marketplace add <source>`), refresh (`marketplace update [name]`), remove (`marketplace remove <name>`). After any change send `reload_plugins` to the live run and show "Restart Claude to apply plugin changes" when `error_count > 0` or no run is live.
- `pub(crate) fn plugin_rows(list_json: &Value) -> Vec<DialogRow>`, `pub(crate) fn marketplace_rows(list_json: &Value) -> Vec<DialogRow>`.

- [ ] **Step 1: Probe.** Run `claude plugin list --help`, `claude plugin install --help`, `claude plugin marketplace list --help` and `claude plugin list --json`; save one real `list --json` output as `crates/agent_chat/src/chat_view/fixtures/plugin-list.json` (strip personal paths) and correct the flag names above if they differ.
- [ ] **Step 2: Tests.** `plugin_rows_parse_the_recorded_list` (row count and the enabled flag of the first row match the fixture); `install_builds_arguments_with_scope`.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Manual check.** Disable an installed plugin, confirm its commands vanish from the slash menu after the reload, enable it again.
- [ ] **Step 5:** Commit `feat(agent-chat): manage plugins and marketplaces`.

### Task 28: `/usage`, context breakdown, `/export`, `/copy`

**Files:**
- Modify: `claude_dialogs.rs`, `crates/agent_chat/src/usage_rings.rs:301-330` (context card), `slash_commands.rs`, `HIDDEN_COMMANDS`

**Interfaces:**
- Produces: `usage_sections(response)` (rows per rate-limit window with percent and reset time; session rows: cost, duration, lines added and removed, per-model tokens); `context_sections(response)` (rows per `categories[]` with tokens and share, then `memoryFiles[]`), shown inside the existing `ContextCard`; `AppCommand::Export { file: Option<String> }` (no file: save dialog prefilled with `default_filename`; with file: write relative to the project root, refusing a path that escapes it); `AppCommand::Copy { nth: Option<usize> }` (copies the n-th most recent assistant reply, default 1).

- [ ] **Step 1: Tests.** `usage_rows_show_percent_and_reset`; `context_rows_sum_to_total`; `export_writes_the_returned_text`; `export_refuses_paths_outside_the_project` (`../x.md` is an error); `copy_nth_picks_the_right_reply`.
- [ ] **Step 2: Implement**; manual check: `/export` saves a readable transcript; `/copy 2` puts the previous reply on the clipboard; the context card lists categories.
- [ ] **Step 3:** Commit `feat(agent-chat): add usage, context breakdown, export and copy commands`.

### Task 29: Sign in and sign out

**Files:**
- Modify: `crates/agent_chat/src/usage_rings.rs:443-560` (accounts card: replace the "sign in through the CLI" text at ~554), `claude_dialogs.rs`, `slash_commands.rs`

**Interfaces:**
- Produces: `AppCommand::Login`, `AppCommand::Logout`. Login: `control_once` with `claude_authenticate{loginWithClaudeAi:true}`, open `automaticUrl` in the browser, show a field for a pasted code (split at `#` into `authorizationCode` and `state`, sent as `claude_oauth_callback`), and await `claude_oauth_wait_for_completion`; on success refresh accounts and usage. Logout runs `claude auth logout`. A run that ends with "Not logged in" shows a "Sign in" button on the notice.
- The three requests must go to one process: add `ClaudeHarness::control_session(cwd) -> ControlSession` with `async fn call(&mut self, request: Value) -> Result<Value, HarnessError>` and build `control_once` on it.

- [ ] **Step 1: Tests.** Harness scenario `login`: `login_flow_sends_three_requests_to_one_process`; `pasted_code_is_split_at_hash` (`"abc#xyz"` gives code `abc`, state `xyz`; no `#` gives state from the earlier response or an error). 
- [ ] **Step 2: Implement.**
- [ ] **Step 3: Manual check.** With a spare saved account slot: `/logout`, then `/login`; the browser opens, and after approval the usage rings return.
- [ ] **Step 4:** Commit `feat(agent-chat): sign in and out of Claude from the chat`.

### Task 30: `/btw` side questions

**Files:**
- Modify: `crates/agent_chat/src/chat_view/side_pane.rs`, `slash_commands.rs`, `crates/agent_chat/src/session.rs` (`side_answers: VecDeque<(String, String)>`, cap 20, not persisted)

- [ ] **Step 1: Tests.** `btw_sends_a_side_question_and_keeps_the_newest_20`; `btw_without_text_opens_the_pane`; `null_response_shows_no_answer_notice`.
- [ ] **Step 2: Implement**: `/btw <question>` sends `side_question{question}` and shows question and answer in the side pane with a trash button; the main transcript is untouched.
- [ ] **Step 3: Manual check.** While a long turn runs, `/btw what file are you editing?`. Expected: an answer appears in the side pane and the turn continues.
- [ ] **Step 4:** Commit `feat(agent-chat): ask side questions with /btw`.

### Task 31: Thinking switch for every model, Ctrl+O

**Files:**
- Modify: `crates/agent_harness/src/claude/catalog.rs:405-438`, `crates/agent_harness/src/claude.rs` (run loop `Steer::Message` settings handling at ~1204), `crates/agent_chat/src/chat_view.rs:2513-2540`, keymaps (`AgentChat`: `ctrl-o`)

**Interfaces:**
- Produces: the Thinking option is offered for every model whose `initialize.models[]` entry has `supportsThinkingOff` (today only Haiku 4.5); a live change sends `set_max_thinking_tokens{max_thinking_tokens: 31999 | 0, thinking_display: "summarized" | null}`; launch uses `--thinking=disabled` when off. Action `agent_chat::ToggleThinkingBlocks` expands or collapses all "Thought process" rows.

- [ ] **Step 1: Tests.** `claude_models.rs`: `thinking_option_follows_supports_thinking_off`. `tests/claude.rs`: `thinking_toggle_is_applied_live` (scenario asserts the control line). `chat_view.rs`: `ctrl_o_expands_all_thinking_rows`.
- [ ] **Step 2: Implement**; `cargo test -p agent_harness thinking`.
- [ ] **Step 3:** Commit `feat(agent-chat): toggle thinking for any model that supports it`.

### Task 32: Command menu sections and the hidden-command sweep

**Files:**
- Modify: `crates/agent_harness/src/claude.rs:318-383`, `crates/agent_chat/src/chat_view/completion.rs`, `crates/agent_chat/src/slash_commands.rs`

- [ ] **Step 1: Test.** `no_native_command_is_hidden`: every `AppCommand` name is absent from `HIDDEN_COMMANDS`. `menu_groups_commands_into_sections`: sections in order Customize (MCP servers, Slash commands and skills, Output styles, Hooks, Permissions, Memory, Instructions, Status, Sandbox, Plugins), Context (Export conversation, Bookmarks), Settings (Focus view, Sign out, Report a problem, General config), Model (Switch model, Thinking). `bug_and_feedback_open_the_issue_page`.
- [ ] **Step 2: Implement.** `/bug` and `/feedback` open wu's issue tracker URL (the same one the Help menu uses); "General config" opens the agent chat settings page; `/bookmarks` toggles Saved Messages; `/tasks` opens the tasks card; `/usage`, `/chrome`, `/remote-control` map to their tasks. Commands that stay terminal-only (`/config`, `/ide`, `/vim`, `/terminal-setup`, …) keep the toast.
- [ ] **Step 3: Manual check.** Type `/` in the composer: sections render, each entry opens its dialog.
- [ ] **Step 4:** Commit `feat(agent-chat): organize the command menu and route native commands`.

---

# Phase 7: Session list

### Task 33: Archive and auto-archive

**Files:**
- Modify: `crates/agent_chat/src/session.rs:229-264` (`archived: bool`, `unarchived_at: Option<i64>`; today the old `archived` key is ignored at 6442-6463), `crates/agent_chat/src/agent_panel.rs:1363-1538`, settings

**Interfaces:**
- Produces: `AgentStore::set_archived(&mut self, ids: &[String], archived: bool, cx)`; `pub(crate) fn should_auto_archive(metadata: &SessionMetadata, now: i64, days: u32, is_open: bool, is_grouped: bool) -> bool` = `days > 0 && max(updated_at, unarchived_at) <= now - days*86400 && !is_open && !unseen && !pinned && !is_grouped && not working`; setting `archive_inactive_sessions` (one of 0, 1, 2, 7, 14; default 14). Runs when the list is built and hourly.

- [ ] **Step 1: Tests.** Table test `auto_archive_rules` (older than 14 days archives; open, unseen, pinned, grouped and days 0 do not; a session unarchived yesterday does not). `archived_sessions_move_to_their_own_section_and_back`. `legacy_archived_flag_still_loads` keeps the existing tolerance for old files.
- [ ] **Step 2: Implement.** Row hover action and menu entry "Archive"; a collapsed "Archived" section with per-row "Unarchive" and a header "Unarchive all".
- [ ] **Step 3: Manual check.** Archive a chat, restart wu, it is still under Archived; unarchive it.
- [ ] **Step 4:** Commit `feat(agent-chat): archive chats`.

### Task 34: Custom groups and multi-select

**Files:**
- Modify: `crates/agent_chat/src/session.rs:2702` (`ChatListPrefs.groups: HashMap<String, Vec<ChatGroup>>` keyed by project root), `crates/agent_chat/src/agent_panel.rs`

**Interfaces:**
- Produces: `pub struct ChatGroup { pub id: String, pub name: String, pub collapsed: bool, pub session_ids: Vec<String> }`; `AgentStore::{create_group(root, name, ids), rename_group, delete_group, move_to_group(ids, group_id: Option<&str>)}`; at most 100 groups per project; a session is in at most one group; deleting a group keeps its chats.

- [ ] **Step 1: Tests.** `moving_a_chat_between_groups_removes_it_from_the_old_one`; `deleting_a_group_keeps_its_chats`; `group_limit_is_100`; `shift_click_selects_a_range_and_ctrl_click_toggles`; `deleted_chats_are_dropped_from_groups`.
- [ ] **Step 2: Implement.** Row menu: "Move to group" (existing groups, "New group…", "Remove from group"); group header menu: Rename, Delete; drag is not required. Menu actions apply to the whole selection.
- [ ] **Step 3: Manual check.** Select three chats with Shift-click, "New group…", name it; restart wu; the group persists.
- [ ] **Step 4:** Commit `feat(agent-chat): group chats in the list`.

### Task 35: Filters, mark as unread, tab status dot

**Files:**
- Modify: `crates/agent_chat/src/agent_panel.rs:1190-1204` and row rendering at ~1649, `crates/agent_chat/src/chat_view.rs` (tab content), `crates/agent_chat/src/agent_chat.rs` (action `MarkChatUnread`), `session.rs` (`mark_unseen`)

**Interfaces:**
- Produces: `pub(crate) enum ChatFilter { NeedsInput, Working, Completed, Open, Closed }`, `pub(crate) fn matches_filters(row: &RowState, active_only: bool, filters: &[ChatFilter]) -> bool` (several filters are OR-ed; "Active" = working or needs input or open); tab dot: blue when a question is pending, orange when finished and unseen.

- [ ] **Step 1: Tests.** Table test `filter_matching`; `mark_unread_sets_unseen_and_survives_reload`; `tab_dot_is_blue_for_pending_and_orange_for_unseen`.
- [ ] **Step 2: Implement** (an "Active" toggle and a funnel menu in the panel header; also give `group_by_project`, `sort`, `compact_rows`, `all_projects` their missing menu entries in that funnel, since they have no UI today).
- [ ] **Step 3:** Commit `feat(agent-chat): filter the chat list and mark chats unread`.

### Task 36: Titles in sync with the CLI, reopen closed chat, shortcuts

**Files:**
- Modify: `crates/agent_chat/src/session.rs` (rename path), `crates/agent_harness/src/claude/history.rs` (append helper), `crates/agent_harness/src/claude/normalize.rs` (`session_title_changed` to `TitleChanged`), `crates/agent_chat/src/agent_chat.rs`, keymaps, settings

**Interfaces:**
- Produces: `pub fn append_custom_title(config_dir: &Path, cwd: &Path, session_id: &str, title: &str) -> std::io::Result<()>` appending one line `{"type":"custom-title","sessionId":…,"customTitle":…}` to the session's `.jsonl`; renaming a chat calls it and, when a run is live, `rename_session{title,source:"host",session_id}`. Untitled chats request `generate_session_title{description:<first prompt>,persist:true}` after the first turn. Actions `agent_chat::FocusInput` (toggles focus between the editor and the last chat composer; `ctrl-escape` / `cmd-escape`), `agent_chat::NewChatInTab` (`ctrl-shift-escape` / `cmd-shift-escape`), `agent_chat::ReopenClosedChat`, `agent_chat::FocusLastMessage`. Settings `enable_new_conversation_shortcut` (default false; binds `ctrl-n`/`cmd-n` in `AgentChat`), `enable_reopen_closed_session_shortcut` (default true).

- [ ] **Step 1: Tests.** `history.rs`: `custom_title_line_is_appended_and_read_back` (the existing reader returns the new title). `session.rs`: `rename_updates_the_cli_transcript`; `reopen_closed_chat_restores_the_last_closed_tab`; `focus_input_toggles_between_editor_and_composer`. Keymap: `script/check-keymaps` passes.
- [ ] **Step 2: Implement.** On Windows `ctrl-escape` is taken by the Start menu, so also bind `ctrl-alt-escape` there and say so in the action's doc comment.
- [ ] **Step 3: Manual check.** Rename a chat in wu, run `claude --resume` in a terminal: the picker shows the new title.
- [ ] **Step 4:** Commit `feat(agent-chat): sync chat titles with Claude Code and add chat shortcuts`.

### Task 37: Continue an interrupted turn after restart

**Files:**
- Modify: `crates/agent_chat/src/session.rs` (persist `interrupted_at: Option<i64>` in metadata when wu quits mid-turn), `crates/agent_harness/src/proto.rs` (`RunRequest.resume_interrupted: bool`), `crates/agent_harness/src/claude.rs:157-224`, settings

**Interfaces:**
- Produces: setting `continue_after_reload` (default true). When a chat that was working at quit is reopened within 10 minutes, wu starts a run with `resume` set, an empty prompt suppressed, and env `CLAUDE_CODE_RESUME_INTERRUPTED_TURN=1`.

- [ ] **Step 1: Probe.** `grep -o '.\{300\}CLAUDE_CODE_RESUME_INTERRUPTED_TURN.\{300\}' …/extension.js | head -3` to confirm how the extension starts that run (what, if anything, it writes to stdin) and the max-age variable's unit; mirror it.
- [ ] **Step 2: Tests.** `interrupted_chat_resumes_once_within_the_window`; `older_interruptions_are_left_alone`; `setting_off_never_resumes`.
- [ ] **Step 3: Implement**; manual check: start a long task, quit wu, reopen: the chat shows "Continuing…" and finishes.
- [ ] **Step 4:** Commit `feat(agent-chat): continue an interrupted turn after restart`.

---

# Phase 8: Composer and transcript

### Task 38: Prompt suggestions

**Files:**
- Modify: `crates/agent_harness/src/claude/wire.rs:5` (`Frame::PromptSuggestion`) and `:390` (`"promptSuggestions": bool` in `initialize`), `normalize.rs`, `proto.rs` (`AgentEvent::PromptSuggestion { suggestion: String }`), `crates/agent_chat/src/chat_view/composer.rs`, settings, keymaps (`AgentComposer`: `tab` when the composer is empty and a suggestion is shown)

**Interfaces:**
- Produces: setting `prompt_suggestions` (default true); `RunRequest.prompt_suggestions: bool`; the suggestion is shown as placeholder text in the empty composer; Tab accepts it into the composer (not sent); typing or a new turn clears it; a hidden chat sends `set_prompt_suggestions_paused{paused:true}` and `false` when shown.

- [ ] **Step 1: Tests.** `wire.rs`: `parses_prompt_suggestion_frames`. `chat_view.rs`: `tab_accepts_the_suggestion_only_when_empty`; `typing_clears_the_suggestion`. Harness scenario `suggest`.
- [ ] **Step 2: Implement**; manual check: after a turn ends a gray suggestion appears; Tab fills it.
- [ ] **Step 3:** Commit `feat(agent-chat): show suggested next prompts`.

### Task 39: Task controls (stop one task, run in background, live output, agent chip)

**Files:**
- Modify: `crates/agent_chat/src/chat_view/background_tasks.rs:237-274`, `crates/agent_harness/src/claude/normalize.rs` (`task_progress`, `task_updated`), `proto.rs` (`AgentEvent::TaskProgress { task_id, total_tokens: Option<u64>, tool_uses: Option<u64>, duration_ms: Option<u64>, last_tool: Option<String> }`), `chat_view/composer.rs:371` (chip), `wire.rs` (`initialize` gains `"perTaskStopAffordance": true`)

**Interfaces:**
- Consumes: `control()` with `stop_task{task_id}`, `background_tasks{tool_use_id}`, `get_task_output{task_id}`.
- Produces: per-task "Stop"; "Run in background" under a running Bash or subagent tool row; the tasks card polls `get_task_output` every 2 s while open (this closes the open item in `context/features/background-tasks.md`); a footer chip "N agents" opening the card with status, elapsed time and tokens.

- [ ] **Step 1: Tests.** `normalize.rs`: `task_progress_becomes_an_event`. `background_tasks.rs`: `stop_sends_stop_task_for_that_id_only`; `background_button_sends_the_tool_use_id`; `output_polling_stops_when_the_card_closes`.
- [ ] **Step 2: Implement**; manual check: ask Claude to run `sleep 120` in the background, open the card, Stop it; the row shows Canceled and the main turn is unaffected.
- [ ] **Step 3:** Commit `feat(agent-chat): stop, background and follow individual tasks`.

### Task 40: Focus view toggle and the settings batch

**Files:**
- Modify: `crates/agent_chat/src/agent_chat.rs`, keymaps (`AgentChat`: `ctrl-alt-f`), `crates/agent_chat/src/chat_view.rs`, `composer.rs`, `crates/agent_harness/src/claude.rs:157-224`, settings

**Interfaces:**
- Produces: action `agent_chat::ToggleFocusView` (flips `compact_transcript` in user settings and sends `apply_flag_settings{settings:{viewMode:"focus"|null}}` to live runs). New settings: `escape_stops` (true; Escape in the composer stops the running turn), `scroll_to_bottom_on_send` (true), `show_message_timestamps` (true), `context_usage_threshold` (50; the context chip is hidden below this percent), `spinner_verbs` (`{"mode":"append","verbs":[]}`; `mode` is `append` or `replace`), `environment_variables` (`[]` of `{name, value}`; added to the child env after wu's own), `process_wrapper` (unset; when set the wrapper is spawned with the `claude` path as its first argument), `allow_bypass_permissions` (**true**, see note).
- Note: the extension hides Bypass unless `allowDangerouslySkipPermissions` is on. wu offers it today, so the wu default stays true to avoid removing a feature; turning it off hides `FullAccess` from the picker and from `default_permission`.

- [ ] **Step 1: Tests.** `toggle_focus_view_writes_the_setting_and_notifies_runs`; `escape_stops_only_when_enabled_and_working`; `context_chip_respects_the_threshold`; `claude.rs`: `user_environment_variables_are_applied_last`, `process_wrapper_receives_the_executable_first`; `bypass_is_hidden_when_not_allowed`; `spinner_verbs_append_and_replace`.
- [ ] **Step 2: Implement** each with the Settings recipe; run the three translation tests.
- [ ] **Step 3:** Commit `feat(agent-chat): add focus view toggle and extension-equivalent settings`.

### Task 41: Deep links

**Files:**
- Modify: `crates/wu/src/wu/open_listener.rs:45-171`, `crates/wu/src/main.rs:864-1014`, `crates/agent_chat/src/agent_chat.rs`

**Interfaces:**
- Produces: `OpenRequestKind::AgentOpen { session: Option<String>, prompt: Option<String> }` for `wu://agent/open?session=<id>&prompt=<text>` and `OpenRequestKind::AgentInstallPlugin { plugin: String, marketplace: Option<String> }` for `wu://agent/install-plugin?plugin=…&marketplace=…`; `pub fn open_chat_from_link(session: Option<String>, prompt: Option<String>, window, cx)` in `agent_chat`.
- Rules: `session` longer than 200 characters or containing `/`, `\`, `..` or NUL is rejected with a log line and nothing opens; `prompt` is percent-decoded and placed in the composer, never sent; install-plugin opens the Plugins dialog's install form prefilled and waits for the user to confirm.

- [ ] **Step 1: Tests** (next to the parser tests at `open_listener.rs` ~1411): `parses_agent_open_with_both_parameters`; `open_link_validates_session_and_never_sends` (the four bad session forms are rejected; a valid link leaves the session idle with the draft set); `install_plugin_link_requires_confirmation`. Add the `Debug` arms.
- [ ] **Step 2: Implement**; manual check from PowerShell: `Start-Process "wu://agent/open?prompt=review%20my%20changes"`: a new chat opens with the text waiting in the composer.
- [ ] **Step 3:** Commit `feat(wu): open agent chats from wu:// links`.

### Task 42: Worktrees

**Files:**
- Modify: `crates/agent_chat/src/session.rs:4127-4130` (`create_session` passes the repository's current branch instead of `None`), `crates/agent_chat/src/chat_view.rs` (banner), `crates/agent_chat/src/agent_panel.rs` (new-chat menu)

**Interfaces:**
- Consumes: `git_ui_core::worktree_service::create_worktree_workspace` (`worktree_service.rs:746`).
- Produces: new chats record `branch`; a chat whose `cwd` is a linked git worktree shows a banner "This chat is in worktree {name}" with "Open worktree"; the new-chat menu gains "New Claude chat in a new worktree" which creates the worktree workspace and opens the chat there.

- [ ] **Step 1: Tests.** `new_chats_record_the_current_branch` (FakeFs git repo on branch `feature`); `worktree_banner_shows_only_for_linked_worktrees`.
- [ ] **Step 2: Implement**; manual check: create a chat in a new worktree, ask it to edit a file: the change lands in the worktree directory, not the main checkout.
- [ ] **Step 3:** Commit `feat(agent-chat): run chats in git worktrees`.

### Task 43: Small transcript and input items

**Files:**
- Modify: `crates/agent_harness/src/claude/normalize.rs:184-193`, `crates/agent_chat/src/chat_view.rs`, `chat_view/attachments.rs:217`, `chat_view/composer.rs`, settings

**Interfaces and items:**
- `NotebookEdit` decodes to `ToolCall::EditFile{path: notebook_path, old_string: None, new_string: new_source}` so it renders as an edit, not an unknown tool.
- `SendUserFile` tool results render as a clickable file row opening the file.
- Pasted text has invisible Unicode characters removed (zero-width U+200B-U+200F, bidi controls U+202A-U+202E and U+2066-U+2069, tag characters U+E0000-U+E007F), with a one-time notice "Removed N invisible characters". `pub(crate) fn strip_invisible(text: &str) -> (String, usize)`.
- A prompt-cache clock in the footer: minutes left = cache lifetime minus time since the last assistant message; lifetime is 60 min when that message's usage had `cache_creation.ephemeral_1h_input_tokens > 0`, else 5 min; hidden when expired or unknown.
- Setting `use_terminal` (default false): new Claude chats open `claude` in a terminal tab (the existing "continue in terminal" path, `agent_panel.rs:798-830`) instead of a chat view.

- [ ] **Step 1: Tests.** `notebook_edit_renders_as_an_edit`; `send_user_file_row_opens_the_file`; `strip_invisible_counts_and_keeps_visible_text` (`"a\u{200B}b\u{202E}c"` gives `("abc", 2)`; emoji with ZWJ U+200D is kept intact); `cache_clock_minutes` table test; `use_terminal_opens_a_terminal_tab`.
- [ ] **Step 2: Implement** and commit `feat(agent-chat): polish transcript rows and pasted input`.

---

# Phase 9: Remote

### Task 44: Remote Control

**Files:**
- Modify: `slash_commands.rs`, `claude_dialogs.rs`, `crates/agent_chat/src/chat_view/composer.rs` (footer indicator), `HIDDEN_COMMANDS`

- [ ] **Step 1: Tests.** `remote_control_enable_shows_the_session_url` (fixture response with `session_url`); `remote_control_disable_clears_the_indicator`; `startup_toggle_applies_the_flag_setting` (`apply_flag_settings{settings:{remoteControlAtStartup:true}}`).
- [ ] **Step 2: Implement**: `/remote-control` sends `remote_control{enabled:true,name:<chat title>}` and shows a small dialog with the URL (copy, open in browser) and a footer indicator; clicking it disables. The command menu's Settings section gets "Enable Remote Control for all sessions". Shown only when `initialize.remote_control_available`.
- [ ] **Step 3: Manual check.** Enable it, open the URL on a phone, send a message there: it appears in the wu chat.
- [ ] **Step 4:** Commit `feat(agent-chat): add Remote Control`.

### Task 45: Cloud sessions in "Continue Saved Chat"

**Files:**
- Modify: `crates/agent_chat/src/saved_chats.rs:174-245`, `crates/agent_harness/src/claude.rs` (`external_sessions` stays local; new methods below)

**Interfaces:**
- Produces: a "Web" tab in the picker listing `list_cloud_sessions{user_initiated:true}` filtered to the project's GitHub `owner/name` (from the `origin` remote); choosing one pages through `get_cloud_session_transcript`, converts the entries with the existing `history` reader into a new local chat (a copy; the cloud session is not changed), and opens it. Hidden when the account is not a claude.ai login.

- [ ] **Step 1: Probe.** Call both requests once through a temporary debug action against the user's account and save trimmed, anonymized responses as fixtures `cloud-sessions.json` and `cloud-transcript.json`; confirm `entries[]` uses the same line shape as local `.jsonl` files (`type`, `uuid`, `message`). If it does not, write the mapping in this task before the tests.
- [ ] **Step 2: Tests.** `cloud_sessions_are_filtered_to_the_repository`; `cloud_transcript_pages_until_no_cursor`; `imported_chat_is_a_local_copy` (transcript entries match the fixture; no control request other than the two reads is sent).
- [ ] **Step 3: Implement**; manual check: a claude.ai/code session for this repo appears under Web and opens with its history.
- [ ] **Step 4:** Commit `feat(agent-chat): continue Claude cloud sessions locally`.

---

# Phase 10: Plugin-drawn panels

Claude Code plugins ("mods") can draw into the host's UI: they replace or decorate named components (a tool row, the spinner, the area above the prompt), open their own panes, and show toasts and status text. The CLI does the drawing logic; the host asks it for a tree per component and renders that tree. All requests go through the control channel from Task 2.

**Protocol (from the installed extension, v2.1.331).** `client_id` is a UUID per chat view; `surface` names the host.

| Direction | subtype | fields | response |
|---|---|---|---|
| wu to CLI | `ui_attach` | `surface`, `client_id`, `viewport{columns,rows,isFullscreen}`, `answers:["ui_copy","ui_prompt_read","ui_prompt_fill","ui_prompt_suggest"]` | `surfaces[]` |
| wu to CLI | `ui_detach` | `client_id` | `detached` |
| wu to CLI | `ui_render` | `surface`, `client_id`, `component`, `instance_id`, `props`, `viewport?`, `content_rows?`, `keyed?` | `tree`, `props`, `rewritten`, `hooked`, `drawn_by[]` (keep at most 20) |
| wu to CLI | `ui_press` | `surface`, `client_id`, `plugin`, `handle`, `key?`, `href?` | `handled`, `element?` |
| wu to CLI | `ui_input` / `ui_select` | `surface`, `client_id`, `plugin`, `handle`, `value`, `kind?` (input only), `key?`, `component?`, `instance_id?` | `handled`, `element?`, `value?` |
| wu to CLI | `ui_scroll` | `surface`, `client_id`, `component`, `instance_id`, `offset`, `by`, `body_rows`, `content_rows`, `pointer?`, `keyed?` | `moved`, `offset`, `deny?`, `follow_end?` |
| wu to CLI | `ui_focus` | `surface`, `client_id`, `component`, `instance_id`, `is_held`, `element?`, `by?` | `moved`, `element{plugin,key}` |
| wu to CLI | `ui_panes` | `client_id` | `panes[{id,plugin,title,close_on_escape?,hold_toasts?,background_color?,rows?,columns?}]`, `shown_id`, `focused_id`, `focus_requested_id` |
| wu to CLI | `ui_pane_show` / `ui_pane_focus` | `id`, `surface`, `client_id` | `shown_id` / `focused_id` |
| wu to CLI | `ui_close` | `id`, `client_id` | `closed` |
| CLI to wu (control request) | `ui_copy` | `text` | `{copied}` |
| CLI to wu (control request) | `ui_prompt_read` | none | `{text,cursor}` (text at most 1,000,000 chars) |
| CLI to wu (control request) | `ui_prompt_fill` | `text`, `mode` | `{filled}` |
| CLI to wu (control request) | `ui_prompt_suggest` | `text` | `{shown}` |
| CLI to wu (system frame) | `ui_toast` | `plugin`, `text`, `timeout_ms` | - |
| CLI to wu (system frame) | `ui_status` | `plugin`, `text` or null (null clears) | - |
| CLI to wu (system frame) | `ui_log` | `plugin`, `text` | - |
| CLI to wu (system frame) | `ui_invalidate` | `instances[{surface,component,instance_id}]` (absent = everything) | - |
| CLI to wu (system frame) | `ui_panes` | the roster above | - |
| CLI to wu (system frame) | `ui_scroll`, `ui_focus`, `ui_placed` | `client_id`, `component`, `instance_id`, `offset`, `follow_end?` (scroll) | - |

Component names the extension asks for: `AbovePrompt`, `AgentRow`, `AskUserQuestion`, `AssistantMessage`, `CommandOutput`, `Pane`, `Spinner`, `ToolGroup`, `ToolResult`, `ToolUse`, `UserMessage`. Drawing is offered only when the `system init` frame's `capabilities` say so. Request timeout 30 s; a prompt read or fill that takes over 4.5 s answers with an empty result.

### Task 46: Capture the drawing protocol from the real CLI

Two things could not be read from the minified code and must be recorded before any rendering code is written: the shape of a `tree`, and which `surface` names the CLI accepts.

**Files:**
- Create: `crates/agent_chat/src/chat_view/fixtures/plugin_ui/` (`init-capabilities.json`, `attach.json`, `render-tool-use.json`, `render-pane.json`, `panes.json`, `README.md` listing the CLI version and the plugin used)
- Modify: none in product code (a temporary debug action is added and removed inside this task)

- [ ] **Step 1: Probe the code.** `grep -o 'noteCapabilities(e){.\{500\}' ~/.vscode/extensions/anthropic.claude-code-2.1.331-win32-x64/extension.js` and write the capability string that turns drawing on into the README. Then read `claude plugin init --help` and the installed `plugin-authoring` skill (`claude plugin list` shows where skills live) for the documented element kinds.
- [ ] **Step 2: Make a test plugin.** `claude plugin init wu-draw-test` and give it the smallest mod that (a) decorates `ToolUse` with one text line and one button, (b) opens one pane with a text, an input and a select, (c) sets a status and fires a toast. Keep it under `~/.claude/skills/wu-draw-test/`; it is test equipment, not part of the repo.
- [ ] **Step 3: Record.** From a temporary debug action in the dev app, on a live Claude chat send `ui_attach` with `surface: "wu"`, then `ui_render` for `ToolUse` and `Pane`, then `ui_panes`; save each raw response as a fixture and save the `system init` frame's `capabilities`.
- [ ] **Step 4: Decision gate.** If `ui_attach` with `surface: "wu"` is refused or returns no surfaces, **stop and ask the user**: the only other route is announcing wu as `"vscode"`, which the Global Constraints forbid without their say-so. Record the answer in the README.
- [ ] **Step 5:** Write, in the README, the tree grammar actually observed (node kinds, their fields, where `plugin`, `handle`, `key` and `href` appear) and remove the debug action. Commit `test(agent-chat): record Claude plugin drawing fixtures`.

### Task 47: Tree model, attach lifecycle and events

**Files:**
- Create: `crates/agent_chat/src/chat_view/plugin_ui.rs`
- Modify: `crates/agent_harness/src/claude/normalize.rs` (system `ui_*` frames), `crates/agent_harness/src/proto.rs` (`AgentEvent::PluginUi { event: serde_json::Value }`, and `SessionStarted` gains `capabilities: Vec<String>`), `crates/agent_harness/src/claude.rs:1374` (answer `ui_copy`, `ui_prompt_read`, `ui_prompt_fill`, `ui_prompt_suggest` through a new `RunControls.ui_host: Arc<dyn Fn(serde_json::Value) -> oneshot::Receiver<serde_json::Value> + Send + Sync>`), `crates/agent_chat/src/session.rs`

**Interfaces:**
- Consumes: `AgentSession::control` (Task 2), fixtures (Task 46).
- Produces:
```rust
pub(crate) const SURFACE: &str = "wu"; // or the value settled in Task 46
pub(crate) enum UiNode { Text { text: SharedString, style: TextStyleHint }, Button { label: SharedString, target: Target }, Link { label: SharedString, href: String, target: Target }, Input { value: String, kind: Option<String>, target: Target }, Select { value: String, options: Vec<SharedString>, target: Target }, Image { source: String }, Progress { fraction: Option<f32> }, Scroll { children: Vec<UiNode>, offset: u32 }, Group { children: Vec<UiNode>, horizontal: bool }, Unknown { kind: String, text: String } }
pub(crate) struct Target { pub plugin: String, pub handle: String, pub key: Option<String> }
pub(crate) fn parse_tree(tree: &serde_json::Value) -> UiNode; // never fails: unknown kinds become Unknown with their text content; depth capped at 64, node count at 5,000
pub(crate) struct PaneInfo { pub id: String, pub plugin: String, pub title: String, pub close_on_escape: bool, pub rows: Option<u32>, pub columns: Option<u32> }
pub(crate) struct PluginUi { /* client_id, attached, statuses, toasts, panes, shown_pane, cache: HashMap<(String, String), Rendered> */ }
pub(crate) struct Rendered { pub tree: UiNode, pub drawn_by: Vec<String>, pub rewritten: bool }
impl PluginUi {
    pub fn available(capabilities: &[String]) -> bool;
    pub fn attach(&mut self, session: &Entity<AgentSession>, columns: u32, rows: u32, cx: &mut Context<ChatView>) -> Task<anyhow::Result<()>>;
    pub fn detach(&mut self, session: &Entity<AgentSession>, cx: &mut Context<ChatView>);
    pub fn render(&mut self, session: &Entity<AgentSession>, component: &str, instance_id: &str, props: serde_json::Value, cx: &mut Context<ChatView>) -> Option<&Rendered>; // cached; fetches in the background and notifies
    pub fn apply_event(&mut self, event: &serde_json::Value) -> bool; // true when a re-render is needed
}
```
- The exact `UiNode` variants follow the grammar recorded in Task 46; adjust the enum to it and keep `Unknown` as the catch-all.
- `ui_invalidate` drops the listed cache entries (all when `instances` is absent); `ui_status` with null text removes that plugin's status; toasts keep the newest 5 and expire after `timeout_ms`.

- [ ] **Step 1: Tests.** `parse_tree_reads_the_recorded_fixtures` (the `render-tool-use.json` tree yields one `Text` and one `Button` whose `target.plugin == "wu-draw-test"`); `unknown_node_kinds_become_text_not_errors`; `oversized_and_deeply_nested_trees_are_capped` (a 10,000-node tree parses to at most 5,000 nodes, depth 200 stops at 64, no panic, no stack overflow); `not_available_without_the_capability`; `invalidate_drops_only_listed_instances`; `status_null_clears_and_toasts_expire`; `normalize.rs`: `ui_frames_become_plugin_ui_events`; `tests/claude.rs` scenario `ui-host`: `ui_copy` is answered `{"copied":true}` and `ui_prompt_read` returns the composer text with a cursor inside its length; a slow host answers `{}` after 4.5 s.
- [ ] **Step 2: Implement.** `ChatView` attaches when it becomes visible on a session whose capabilities allow it and detaches when hidden or dropped; nothing is sent to sessions without the capability or to other agents.
- [ ] **Step 3: Verify.** `cargo test -p agent_chat plugin_ui` and `cargo test -p agent_harness ui_`. Expected: PASS.
- [ ] **Step 4:** Commit `feat(agent-chat): attach to Claude plugin drawing and model its trees`.

### Task 48: Render plugin content in the transcript and above the prompt

**Files:**
- Modify: `crates/agent_chat/src/chat_view/plugin_ui.rs` (`render_node`), `crates/agent_chat/src/chat_view.rs` (tool rows ~2513-2540, user and assistant rows ~1628 and ~2086, the working spinner row), `crates/agent_chat/src/chat_view/composer.rs` (above the pill; footer status), `crates/agent_chat/src/chat_view/background_tasks.rs` (agent rows)

**Interfaces:**
- Consumes: `PluginUi::render`, `UiNode`, `Target`.
- Produces: `pub(crate) fn render_node(node: &UiNode, on_action: Rc<dyn Fn(UiAction, &mut Window, &mut App)>, cx: &App) -> AnyElement`; `pub(crate) enum UiAction { Press { target: Target, href: Option<String> }, Input { target: Target, value: String, kind: Option<String> }, Select { target: Target, value: String } }` mapped to `ui_press`, `ui_input`, `ui_select`.
- Components wired, with their `instance_id`: `ToolUse` and `ToolResult` (tool id), `ToolGroup` (first tool id of a folded group), `UserMessage` and `AssistantMessage` (entry index as string), `CommandOutput` (entry index), `Spinner` (`"spinner"`), `AgentRow` (task id), `AskUserQuestion` (question id), `AbovePrompt` (`"above-prompt"`). `props` carries what wu already shows for that row (tool name and input, text, status).
- Rule: when `hooked` is false the row renders exactly as today and no label is shown. When a plugin drew or rewrote it, the row shows the tree and a muted caption "From the {plugin} plugin" (several: "From the {a} and {b} plugins"). Plugin text is rendered as plain text, never as markdown or links unless the node is a `Link`; a `Link` or a press with `href` opens only `http`/`https` URLs, after the same confirmation wu uses for links in replies.

- [ ] **Step 1: Tests.** `unhooked_rows_render_unchanged` (snapshot of `summarize` output equal with and without `PluginUi`); `hooked_tool_row_shows_the_tree_and_the_plugin_caption`; `pressing_a_button_sends_ui_press_with_its_target`; `typing_in_an_input_sends_ui_input_and_applies_the_returned_value`; `non_http_hrefs_are_not_opened` (`file:`, `javascript:`, `wu:` are ignored); `status_text_shows_in_the_footer_and_clears_on_null`; `toast_shows_then_expires`.
- [ ] **Step 2: Implement.** Render requests are made only for rows in view; a pending render shows the normal row, then swaps in place (no height jump: reserve the previous height until the tree arrives).
- [ ] **Step 3: Manual check.** With `wu-draw-test` enabled, ask Claude to run `ls`. Expected: the Bash row shows the plugin's line and button with "From the wu-draw-test plugin"; clicking the button triggers the plugin's handler (visible as its toast); disabling the plugin in `/plugins` returns the row to normal after the reload.
- [ ] **Step 4:** Commit `feat(agent-chat): render plugin-drawn rows`.

### Task 49: Plugin panes

**Files:**
- Modify: `crates/agent_chat/src/chat_view/plugin_ui.rs`, `crates/agent_chat/src/chat_view/side_pane.rs:309` (host panes in the existing side pane), keymaps (`AgentSidePane`: `escape`)

**Interfaces:**
- Consumes: `ui_panes`, `ui_pane_show`, `ui_pane_focus`, `ui_close`, `ui_scroll`, `ui_focus`, `ui_render` with component `Pane` and `instance_id` = pane id.
- Produces: `PluginUi::{show_pane(id), focus_pane(id), close_pane(id), scroll_pane(id, by: i32, body_rows: u32, content_rows: u32)}`; the side pane gains a tab per roster entry titled `title` with the caption "From the {plugin} plugin"; `shown_id` decides which pane is visible, `focus_requested_id` moves keyboard focus once; Escape closes a pane only when `close_on_escape`; `background_color` is applied only if it parses as a hex color; `rows`/`columns` set the pane's preferred size in editor cells, clamped to the side pane's bounds; the viewport sent with `ui_render` is the pane's size in cells.

- [ ] **Step 1: Tests.** `roster_updates_add_and_remove_pane_tabs` (from `panes.json`); `shown_id_selects_the_visible_pane`; `escape_closes_only_close_on_escape_panes`; `scrolling_sends_ui_scroll_and_applies_the_returned_offset` (a `deny` reply leaves the offset unchanged); `closing_the_tab_sends_ui_close`; `invalid_background_color_is_ignored`; `panes_vanish_when_the_session_detaches`.
- [ ] **Step 2: Implement.**
- [ ] **Step 3: Manual check.** Trigger the test plugin's pane. Expected: a tab appears in the chat's side pane with the text, input and select; typing and selecting reach the plugin (its status text changes); Escape closes it; reopening the chat tab re-attaches and the pane returns.
- [ ] **Step 4:** Commit `feat(agent-chat): host plugin panes in the chat side pane`.

---

# Final verification

### Task 50: Parity walkthrough

- [ ] **Step 1:** Run the whole suite: `cargo test -p agent_harness`, `cargo test -p agent_chat`, `cargo test -p i18n`, `cargo test -p settings --lib`, `cargo test -p settings_ui --lib`, `./script/clippy -p agent_harness`, `./script/clippy -p agent_chat`, `script/check-keymaps`. Expected: all pass.
- [ ] **Step 2:** Build and run the dev app (`cargo run -p wu`). Walk the Parity Matrix top to bottom with VS Code open beside it on the same project, performing each row in both. Mark each row Pass or Fail in a copy of the matrix in the commit message body.
- [ ] **Step 3:** For each Fail, open a follow-up task in this file under a new "Follow-ups" heading with the observed difference. Do not mark the plan complete while a row fails without a follow-up.
- [ ] **Step 4:** Update `context/features/` with one short note per new area (permissions and plans, edit review, checkpoints, editor context, IDE server, dialogs) following the existing frontmatter convention, and remove the resolved "Open" item from `context/features/background-tasks.md`. Commit `docs(context): describe Claude parity features`.

---

## Parity Matrix

| Extension feature | wu before | Task |
|---|---|---|
| Permission modes Manual / Edit automatically / Auto / Bypass | present | - |
| Plan mode, plan review, comments, `/plan`, `/plan open` | absent | 5, 6, 7 |
| "Always allow" with destination, deny with reason, edit Bash command | absent | 3, 4 |
| Side-by-side diff of a proposed edit, accept / reject, edit the proposal | read-only after the fact | 8, 9 |
| Accept / Reject this change, Accept / Reject Change at Cursor | absent | 10 |
| `autosave` before Read / Edit / Write | absent | 11 |
| Rewind code to here | absent | 12, 13 |
| Fork conversation from here, fork and rewind, `/fork`, `/rewind` | last message only | 14 |
| Selection and open file sent automatically, footer indicator, `attachSelection`, `attachOpenFile`, `respectGitIgnore` | absent | 15 |
| `@file#5-10`, Alt+K "Insert @-Mention Reference" | no line ranges | 16 |
| Explorer / Source Control "Add to Chat" | absent | 17 |
| "Fix with Claude Code", `showFixActions` | absent | 18 |
| "Resolve Merge Conflicts" | absent | 19 |
| `@terminal:name`, PDF attachments | absent | 20 |
| Image paste and drag, attach picker | present | - |
| `getDiagnostics` for panel sessions | absent | 21 |
| IDE server for terminal `claude` (lock file, `openDiff`, selection, `at_mentioned`) | absent | 22, 23 |
| MCP servers dialog, `/mcp` | absent | 25 |
| Hooks, Memory, Instructions, Skills, Status, Sandbox, Output styles | absent | 24 |
| Permissions dialog | absent | 26 |
| Plugins and marketplaces, install link | absent | 27, 41 |
| `/usage`, context breakdown, `/export`, `/copy` | usage rings only | 28 |
| Sign in, switch account, sign out | switch only | 29 |
| `/btw` | absent | 30 |
| Model picker, effort, Ultracode, fast mode | present | - |
| Thinking switch, `Ctrl+O` | Haiku only | 31 |
| Command menu sections | flat list, ~65 hidden | 32 |
| Session history, search, resume CLI sessions, rename, pin, delete | present | - |
| Archive, auto-archive, unarchive all | absent | 33 |
| Session groups, multi-select | project grouping only | 34 |
| Filters (Active, Needs input, Working, Completed, Open, Closed), mark unread, tab dot | partial | 35 |
| AI titles shared with the CLI, Rename Session Tab, Reopen Closed Session, New Conversation, Focus input, Open in New Tab, Focus last message | partial | 36 |
| `continueAfterReload` | absent | 37 |
| Prompt suggestions | absent | 38 |
| Stop a single agent, Run in background, live task output, agent count | display only | 39 |
| Focus view toggle, `escapeStopsClaude`, `scrollToBottomOnSend`, `showMessageTimestamps`, `contextUsageThreshold`, `spinnerVerbs`, `environmentVariables`, `claudeProcessWrapper`, `allowDangerouslySkipPermissions`, `useCtrlEnterToSend`, `initialPermissionMode` | partial | 40 |
| `vscode://anthropic.claude-code/open`, `/install-plugin` | absent | 41 |
| Worktree banner, parallel sessions in worktrees | absent | 42 |
| `useTerminal`, Open in Terminal, `SendUserFile` rows, NotebookEdit display, invisible-character stripping, cache clock | partial | 43 |
| Remote Control | absent | 44 |
| Web tab (cloud sessions) | absent | 45 |
| Plugin-drawn content: decorated rows, above-prompt area, panes, toasts, status, "From the … plugin" label | absent | 46, 47, 48, 49 |
| Message queue, steering, voice dictation, bookmarks, in-chat search, notifications, Claude in Chrome, commit messages, CLI auto-update | present (some beyond the extension) | - |

## Deferred, with reasons (not in this plan)

| Extension feature | Why it is not planned here |
|---|---|
| LaTeX math rendering | GPUI has no math layout; needs a new typesetting dependency. That is a dependency decision for the user, then its own plan. |
| "Add Cell to Chat", `executeCode`, notebook autosave rules | wu has no notebook editor. Only the `NotebookEdit` row display is covered (Task 43). |
| Agent chats in remote (SSH) projects | Needs the CLI to run on the remote host through wu's remote server; a separate architecture change. |
| Screen reader announcements | GPUI exposes no accessibility tree on Windows today. "Focus last message" is covered (Task 36). |
| Onboarding checklist and walkthrough, `hideOnboarding` | Content and product decisions, not protocol work. |
| `lockEditorGroups`, `preferredLocation`, `usePythonEnvironment`, Open VSX / dev container notes, `disableLoginPrompt`, cloud gateway sign-in | VS Code specific, or already covered by wu's own docking settings (`dock`, `chat_panel_dock`). |
| `/workflows`, "Let Claude wait for a subagent" | Not dissected; add after Task 39 if the walkthrough in Task 50 shows a gap. |
