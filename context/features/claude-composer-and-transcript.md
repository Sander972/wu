---
type: feature
status: active
area: agent_chat
tags: [claude, composer, transcript, settings]
---

# Claude composer and transcript extras

Smaller chat behaviors added for Claude Code parity: prompt suggestions, per-task controls, the extension's settings, `wu://` links, worktree chats and transcript polish. Code: `crates/agent_chat/src/chat_view/composer.rs` and `session.rs` (suggestions), `crates/agent_chat/src/chat_view/background_tasks.rs` (tasks), `crates/agent_chat/src/agent_chat_settings.rs` (settings), `crates/wu/src/wu/open_listener.rs` and `agent_chat.rs` (links), `crates/agent_chat/src/agent_panel.rs` and `chat_view.rs` (worktrees), `crates/agent_chat/src/chat_view/attachments.rs` and `crates/agent_harness/src/claude/normalize.rs` (polish).

## Behavior

- Prompt suggestions (`prompt_suggestions`, default true): after a turn a gray suggestion appears in the empty composer, Tab fills it without sending, typing clears it. They pause only while the chat is hidden (a blurred window does not count), and a suggestion is dropped when the agent starts or resumes a turn on its own.
- Background tasks, see [[background-tasks]]: each task row has Stop (`stop_task`, only that task), a running Bash row has "Run in background", and the card tails live output. The chip counts background tasks only.
- Extension settings, all under `agent_chat`: `escape_stops`, `scroll_to_bottom_on_send`, `show_message_timestamps`, `context_usage_threshold`, `spinner_verbs`, `environment_variables`, `process_wrapper`, `allow_bypass_permissions`, `use_terminal`, and `send_with: "modifier_enter"` for Ctrl/Cmd+Enter. Focus view toggles with Ctrl+Alt+F or `/focus` and is saved with `settings::update_settings_file`.
- Launch settings (`ClaudeLaunch` in `proto.rs`): environment variables are added after wu's own, except `CLAUDE_CODE_SSE_PORT` and `CLAUDE_CODE_RESUME_*`, which wu controls. The process wrapper runs with the path of `claude` as its first argument and never through a shell. They also apply to control and probe launches and to commit messages, and they are part of the discovery cache key.
- `wu://agent/open?session=<id>&prompt=<text>` opens a chat with the prompt waiting in the composer (never sent), and `wu://agent/install-plugin?plugin=<name>&marketplace=<name>` asks for confirmation before running `claude plugin install`. The prompt is capped at 10,000 characters with no control characters, a session id must be safe, and a plugin or marketplace name cannot start with `-` or hold whitespace. A link never resumes a cut-off turn or starts a run.
- Worktree chats: `NewClaudeChatInWorktree` (command palette and the new chat menu) creates a git worktree and starts a Claude chat in it. A banner shows only in linked worktrees, named after the parent folder for worktrees wu created (`<dir>/<name>/<repo>`). Failures show a toast.
- Polish: `use_terminal` opens new Claude chats as `claude` in a terminal tab, `SendUserFile` rows open the file, `NotebookEdit` renders as an edit, pasted invisible characters are stripped with one notice (a joiner between emoji parts stays), and the footer shows a prompt cache countdown ("Cache 4m") from real usage frames only, keeping the 1 hour TTL once seen.

## Not done

- Whether foreground parallel subagents should count in the "N agents" chip is an open question for the user.
- The main Stop still ends the whole run when wu knows of background tasks.
- A queued message's editor context is invisible, and Ctrl+O now toggles thinking rows, see [[claude-dialogs-and-commands]].
- The `vscode://anthropic.claude-code/open` scheme is not claimed, only `wu://`.
