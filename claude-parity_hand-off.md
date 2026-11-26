# Claude VS Code parity: hand-off

Status on 2026-11-27. Plan: `docs/superpowers/plans/2026-11-25-claude-vscode-parity.md` (50 tasks). Executor ledger (git-ignored, the detailed record of every review, ruling and deferred finding): `.superpowers/sdd/2026-11-25-claude-vscode-parity/progress.md`.

## Where we are

- **All 50 tasks are implemented, reviewed and committed on `main`**, plus a phase-end check after every phase and a final whole-branch review with one fix wave. Nothing is pushed.
- The only open plan items are Task 50 Steps 2-3: the side-by-side walkthrough of the Parity Matrix in the dev app against VS Code, and adding follow-ups for any row that fails. These need a human at the GUI.
- Final verification at `391bba0`: non-test `cargo check` of all touched crates is clean; `agent_chat` 399/399; `agent_harness` lib 298 and `claude` 40; wu `open_listener` 30; project_panel 137; git_ui 178; terminal 109; i18n, settings and settings_ui pass; `check-keymaps` passes; clippy shows only the two pre-existing warnings below.
- Feature notes for every new area are in `context/features/claude-*.md`.

## What was built (by phase)

| Phase | Tasks | Features |
|---|---|---|
| 0 Foundations | 1-3 | fake-CLI tests run on Windows; request/response control channel; typed permission prompts and decisions |
| 1 Permissions and plan mode | 4-7 | always-allow with destination, deny with reason, editable Bash command; Plan mode; plan approval card; plan comments, Open plan, `/plan` |
| 2 Reviewing edits | 8-11 | proposed-edit diff tab with Accept/Reject, per-hunk decisions; save dirty buffers before Claude reads or edits |
| 3 Checkpoints | 12-14 | file checkpoints and prompt ids; rewind code to a message; fork from any message, fork and rewind, `/fork`, `/rewind` |
| 4 Editor context | 15-20 | selection and open file sent automatically; `@file#5-10` and Alt+K; Add to Chat; Fix with Claude; Resolve Merge Conflicts; `@terminal:` mentions and PDF attachments |
| 5 IDE tools | 21-23 | in-process `getDiagnostics` MCP; WebSocket IDE server and lock file for terminal `claude`; `openDiff`, selection notifications, `CLAUDE_CODE_SSE_PORT` |
| 6 Dialogs and commands | 24-32 | `/status`, `/hooks`, `/memory`, `/skills`, `/sandbox`, output styles, `/mcp`, `/permissions`, `/plugins`, `/usage`, context breakdown, `/export`, `/copy`, `/login`, `/logout`, `/btw`, thinking switch and Ctrl+O, sectioned command menu |
| 7 Session list | 33-37 | archive and auto-archive, custom groups and multi-select, filters, mark unread, tab status dot, titles synced with the CLI, reopen closed chat, chat shortcuts, continue an interrupted turn after restart |
| 8 Composer and transcript | 38-43 | prompt suggestions, per-task Stop / run in background / live output, Focus view and the settings batch (Escape stops, env vars, process wrapper, bypass switch, ...), `wu://agent/...` links, worktree chats, transcript polish (NotebookEdit, SendUserFile, invisible-character stripping, cache clock, `use_terminal`) |
| 9 Remote | 44-45 | Remote Control, cloud sessions in Continue Saved Chat (Web tab) |
| 10 Plugin drawing | 46-49 | recorded protocol fixtures, tree model and attach lifecycle, plugin-drawn rows, plugin panes in the side pane |

## Before you push

- Behavior change: wu chats force thinking on (`alwaysThinkingEnabled: true` via flag settings) when the Thinking option is On or unset, which overrides `alwaysThinkingEnabled: false` in your `~/.claude/settings.json`. Pick Off in the model picker to disable it.
- Behavior change: Deny on a Claude permission card stops the turn (`interrupt: true`), as the extension does.
- Plugin drawing announces wu as `surface: "vscode"` (your decision, temporary): the CLI only accepts `desktop`, `mobile` or `vscode`. It is one constant, `SURFACE` in `crates/agent_chat/src/chat_view/plugin_ui.rs`. Nothing else identifies wu as VS Code.
- Security: the `origin` remote URL contains a plaintext GitHub token. Rotate it and switch to a credential helper.
- `crates/agent_chat/src/chat_view/dictation.rs` and `diff_view.rs` show as modified with line-ending-only differences (content equal to HEAD). Restore them with `git checkout -- <file>` (my attempt was blocked by the auto-mode safety check).
- `~/.claude/dev-mods/1d05f56d-…/` is an empty folder the CLI created during the Task 46 probe; safe to delete.

## Rulings that change the plan text

Recorded in the plan's Progress section and in the ledger.

- `HIDDEN_COMMANDS` must contain every native `AppCommand` name (the plan said to remove them). Enforced by `claude_never_shadows_a_wu_command`.
- Task 14: forks keep the existing `"<title> (2)"` naming.
- Task 24: the output-style picker needs a live run.
- Task 25: `claude mcp add/remove` use the extension's argument order (`--scope S --transport T -- name target...`).
- Task 26: permission rules are edited with `claude edit-permission-rules --json`, because `update_settings` rejects `permissions`.
- Task 29: `claude_oauth_wait_for_completion` is sent before the pasted-code callback, as the extension does.
- Task 33: the first auto-archive pass waits 60 s after launch so restored tabs are not archived.
- Task 48: plugin links open without a confirm prompt, like reply links.
- Process: focused tests per task; full suites and clippy once per phase; a non-test `cargo check` before every commit (added after Task 40 broke the normal build once).

## Known pre-existing issues (not from this work)

- clippy `redundant_clone` at `crates/agent_harness/src/usage.rs:171` and a warning at `crates/workspace/src/status_bar.rs:220`.
- Flaky `agent_chat` test `send_now_shows_the_message_and_queue_order_survives_idle`: fails under full-suite load, passes alone.
- `crates/agent_harness/tests/opencode.rs` fails on Windows because its tests use cwd `/tmp` (= `E:\tmp`, which no longer exists). Recreate `E:\tmp` or make the tests use a temp dir.
- Drive E: fills up: `target/debug/incremental` grows to 100+ GB and `cargo trim` does not shrink it. Delete that folder when space runs low (it is a rebuild cache).

## Not verified against the real CLI or account

- The autosave pre-tool hook (Task 11), Remote Control enable/disable (Task 44) and the cloud Web tab (Task 45): the probes could not or must not run against your account. Check them by hand.
- Whether a one-off `control_once --resume` (export, `/mcp`, context card, `/btw`, rewind on an idle chat) appends anything to the chat's real transcript.
- Orphaned `claude.exe` processes after wu exits on Windows.

## Manual GUI checks still to run

Run in the dev app (`cargo run -p wu`). Automated tests cover the logic; these confirm the UI.

- Task 4: Ask mode, ask Claude to run `git status`, pick "Always allow for this project (just you)": `.claude/settings.local.json` gains the rule and a second `git status` is not asked.
- Task 6: Plan mode, "add a --version flag to script/clippy": no file changes, the plan card appears; "keep planning" with feedback makes Claude revise; "Yes, and auto-accept edits" switches to Accept Edits and Claude edits.
- Task 7: comment on a sentence of a plan, press "No, keep planning": Claude addresses the quote; `/plan open` opens the plan file.
- Task 9: Ask mode, "rename the function foo in <file>": a diff tab opens; hand-edit a proposed line, Accept: the file on disk has the hand-edit.
- Task 10: a two-place edit: reject one hunk, accept the other: only the accepted place changes.
- Task 11: type in a file without saving, ask what is on line 1: the answer shows the unsaved text and the tab is no longer dirty.
- Task 13: two prompts that each edit a file, rewind code to the second: its edit is gone, the first stays.
- Task 14: three-prompt chat, "Fork conversation from here" on prompt 2: a new tab with prompt 1's exchange and prompt 2 in the composer.
- Task 15: select 3 lines, ask "what does this do?": the chip reads "3 lines selected" and the answer is about them.
- Task 16: select lines 5-10, Alt+K: the composer gains `@path#5-10 ` and focus.
- Task 17: right-click two files, Add to Chat: two mentions in the composer.
- Task 18: a type error in a Rust file, open the lightbulb: "Fix with Claude" fills the composer and sends nothing.
- Task 19: a conflicting merge in a scratch repo, Resolve Merge Conflicts: the prompt lists exactly the conflicted files.
- Task 20: `cargo --version` in a terminal titled "build", ask "what version is in @terminal:build?": the version is quoted.
- Task 21: with a compile error, ask Claude to use the ide diagnostics tool: an `ide / getDiagnostics` call names the error.
- Task 23: in a wu terminal `echo $CLAUDE_CODE_SSE_PORT` prints a port; `claude` then `/ide` reports Wu; an edit opens a visible wu diff tab; Accept writes the file; selecting text shows "N lines selected" in the CLI.
- Task 24: `/memory` lists memory files and opens one; `/hooks` lists hooks; `/status` shows version and account.
- Task 25: `/mcp`: disable a server, Claude cannot use it, enable it again; add and remove a server; Authenticate a needs-auth HTTP server.
- Task 26: `/permissions`: add `Bash(git status)` for "just you", it lands in `.claude/settings.local.json` and shows after reopening; remove it.
- Task 27: disable a plugin, its commands vanish from the slash menu; enable it again.
- Task 28: `/export` writes a readable transcript; `/export notes/x.md` works once then refuses; `/export ../x.md` is refused; `/copy 2` copies the previous reply; the context card lists categories; `/usage` shows windows with reset times.
- Task 29: with a spare account slot, `/logout` then `/login`: the browser opens and after approval the usage rings return.
- Task 30: during a long turn, `/btw what file are you editing?`: an answer appears in the side pane and the turn continues.
- Task 31: toggle Thinking off and on mid-chat; Ctrl+O expands and collapses all thought rows.
- Task 32: type `/`: sections render; `/bug` opens the issue page, `/bookmarks` shows Saved Messages, `/tasks` opens the card.
- Task 33: archive a chat, restart wu: it stays under Archived; unarchive it.
- Task 34: select three chats with Shift-click, "New group…", name it; restart wu: the group persists.
- Task 35: the header Active toggle and funnel menu filter the list; row "Mark as unread" sticks; tab dots are blue for a pending question and orange for a finished unseen chat.
- Task 36: rename a chat in wu, run `claude --resume` in a terminal: the picker shows the new title; Ctrl+Shift+T reopens a closed chat; Ctrl+Esc (Windows: Ctrl+Alt+Esc) toggles focus.
- Task 37: start a long task, quit wu, reopen within 10 min: the chat shows "Continuing…" and finishes; reopening again does not resume twice.
- Task 38: after a turn ends a grey suggestion appears in the empty composer; Tab fills it (not sent); typing clears it.
- Task 39: ask Claude to run `sleep 120` in the background, open the tasks card, Stop it: the row shows Canceled and the main turn continues; "Run in background" on a running Bash row.
- Task 40: Ctrl+Alt+F and `/focus` toggle Focus view; Escape in the composer stops a working turn; with `allow_bypass_permissions` off Full Access is gone; a wrapper script that execs its first argument still runs a chat.
- Task 41: `Start-Process "wu://agent/open?prompt=review%20my%20changes"` opens a new chat with the text in the composer, not sent; a bad session id opens nothing.
- Task 42: "New Claude chat in a new worktree" (also in the command palette), ask it to edit a file: the change lands in the worktree; the banner shows only there.
- Task 43: paste text with zero-width characters: they are stripped with one notice; NotebookEdit renders as an edit; a SendUserFile row opens the file; the footer cache clock counts down; `use_terminal` opens new Claude chats in a terminal tab.
- Task 44: `/remote-control` in a running chat, open the link on a phone, send a message there: it appears in wu; clicking the chip turns it off.
- Task 45: a claude.ai/code session for this repo appears under the Web tab and opens with its full history as a local copy.
- Task 48: with a test plugin that decorates ToolUse, ask Claude to run `ls`: the row shows the plugin's drawing with "From the … plugin"; its button works; disabling the plugin restores the row.
- Task 49: a plugin pane appears as a side-pane tab with working inputs; Escape closes only `close_on_escape` panes; reopening the chat tab re-attaches and the pane returns.

## How to finish

1. Run the manual checks above and the Task 50 walkthrough (Parity Matrix at the end of the plan) side by side with VS Code; add a "Follow-ups" heading to the plan for any row that differs.
2. Review the ledger's deferred minor findings (`grep "minor (deferred)" .superpowers/sdd/2026-11-25-claude-vscode-parity/progress.md`) and pick any you want fixed.
3. Push when satisfied; the ledger folder can then be deleted.
