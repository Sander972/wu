---
type: feature
status: active
area: agent_chat
tags: [claude, slash-commands, dialogs]
---

# Claude dialogs and slash commands

The slash commands that open settings-style screens in the Claude Code extension have native wu equivalents: modal dialogs plus a sectioned command menu. Code: `crates/agent_chat/src/chat_view/claude_dialogs.rs` (`ClaudeDialog`, `DialogSection`, `RowAction`, `LoginDialog`, `/export`), `crates/agent_chat/src/slash_commands.rs` (`AppCommand`, sections, `/wu:` renaming), `crates/agent_harness/src/claude.rs` (`HIDDEN_COMMANDS`).

## Commands

- Customize section: `/mcp`, `/skills`, `/output-style`, `/hooks`, `/permissions`, `/memory`, `/status`, `/sandbox`, `/plugins`. Context section: `/export`, `/bookmarks`. Settings section: `/logout`, `/bug`, `/config`, `/focus`, remote control at startup. Model section: `/model`. Plain commands: `/usage`, `/copy [n]`, `/btw`, `/login`, `/tasks`, `/remote-control`, `/plan`, `/fork`, `/rewind`, `/new`, `/resume`, `/stop`, `/diff`, `/files`, `/terminal`.
- Dialogs fetch their rows from the CLI with control requests (`mcp_status`, `get_settings`, `list_permission_rules`, `reload_plugins`, `reload_output_styles`) or by running `claude mcp ...` and `claude plugin ...` for changes. Permission rules are written through the CLI, to the destination the user picks.
- `/export [file]` writes a readable transcript, refusing a path outside the project folder or an existing file. `/copy [n]` copies the nth last reply. `/usage` shows the usage windows with reset times, including model-scoped `limits[]` windows, and a context breakdown by category.
- `/login` and `/logout` run the sign-in flow in a dialog. An `authentication_failed` reply from the CLI shows a not-signed-in notice with a Sign in button. `/logout` signs the user out, so it was never exercised against the real account.
- `/btw` answers a side question in the side pane while the turn continues. It waits up to five minutes and closing the chat cancels pending questions.
- Thinking is a switch in the model picker for models whose capabilities include `supportsThinkingOff`. Ctrl+O (`ToggleThinkingBlocks`, in the `AgentChat` context) expands or collapses every thought row.

## Decisions

- `HIDDEN_COMMANDS` is inverted from what the plan said. It hides the CLI's built-in commands from the menu, and a visible CLI command with the same name as a native `AppCommand` would push wu's command to `/wu:<name>`. So every native command name must be IN `HIDDEN_COMMANDS` (kept alphabetical), enforced by the `claude_never_shadows_a_wu_command` test and `every_native_command_owns_its_name`. Hiding a name the CLI lacks is harmless, and user-defined commands still show.
- The output style picker needs a live run (it applies `outputStyle` with `apply_flag_settings`), so without one it is disabled with a note instead of persisting through `--settings`.
- MCP add and remove follow the extension's argv (`--scope s --transport t -- name target...`, stdio commands split on whitespace). Authenticate and Reconnect are offered only with a live run.
- With Thinking on or unset wu forces `alwaysThinkingEnabled: true` in the flag settings, overriding a user's `~/.claude` value of false, so the picker's default is truthful.

## Limits

- On Windows and Linux, Ctrl+O in a chat no longer opens a file (`workspace::OpenFiles`), a release-note item.
- The output style note is computed when the dialog loads and can go stale while it is open.
- A second concurrent `/btw` answer is plain text, and "Open in Tab" on the side pane only closes it.
- Enter does not submit the MCP add form and Escape closes the whole dialog.
