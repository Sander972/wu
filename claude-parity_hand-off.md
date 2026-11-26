# Claude VS Code parity: hand-off

Status on 2026-11-26. Plan: `docs/superpowers/plans/2026-11-25-claude-vscode-parity.md` (50 tasks). Executor ledger (git-ignored, the detailed record): `.superpowers/sdd/2026-11-25-claude-vscode-parity/progress.md`.

## Where we are

- Done, reviewed and committed on `main`: **Tasks 1-32** (Phases 0-6) plus the Phase 6 end check. 55 local commits from `883a2f1` to `edb7582`. Nothing is pushed.
- Next: **Task 33** (Phase 7, archive and auto-archive). Remaining: Tasks 33-50 and the phase-end checks after Tasks 37, 43, 45 and 49.
- Last verification at `b9a8bb3`: `agent_harness` all suites pass; `agent_chat` 292 pass plus the known flaky test (passes alone); i18n, settings, settings_ui, project_panel, git_ui, terminal and `check-keymaps` pass; clippy clean in plan code.

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

## Before you push

- `Cargo.lock` holds new lock entries for `agent_chat` dependencies added by this work (`git_ui_core`, `buffer_diff`, `lsp`, `wu_actions`, `terminal`, `async-tungstenite`, `smol`, `sysinfo`). They are mixed with your own uncommitted dependency bump, so they were not committed: commit them together with it.
- The work was built against your uncommitted root `Cargo.toml` (e.g. `async-tungstenite = "0.35"`); a reviewer checked the 0.33 API used is compatible.
- Behavior change: wu chats now force thinking on (`alwaysThinkingEnabled: true` via flag settings) when the Thinking option is On or unset, which overrides `alwaysThinkingEnabled: false` in `~/.claude/settings.json`. Pick Off in the model picker to disable it. Commit `25624f4`; it was verified by tests but not by a separate reviewer.
- Behavior change: Deny on a Claude permission card now stops the turn (`interrupt: true`), as the extension does.
- Security: the `origin` remote URL contains a plaintext GitHub token. Rotate it and switch to a credential helper.

## Rulings that change the plan text

Recorded in the plan's Progress section and in the ledger.

- `HIDDEN_COMMANDS` must contain every native `AppCommand` name (the plan said to remove them). Enforced by `claude_never_shadows_a_wu_command`.
- Task 25: `claude mcp add/remove` use the extension's argument order (`--scope S --transport T -- name target...`).
- Task 26: permission rules are edited with `claude edit-permission-rules --json`, because `update_settings` rejects `permissions`.
- Task 29: `claude_oauth_wait_for_completion` is sent before the pasted-code callback, as the extension does.
- Task 14: forks keep the existing `"<title> (2)"` naming.
- Process (from Task 27): focused tests per task; full suites and clippy once per phase; Sonnet by default, Opus for risky tasks; next task may start while the previous is in review.

## Carry-overs for the remaining tasks

- Task 40: add "Focus view" to the command menu's Settings section.
- Task 44: replace the `/remote-control` "terminal only" toast.
- Task 46: if `ui_attach` with `surface: "wu"` is refused, stop and ask (announcing wu as `vscode` is forbidden without your approval).
- Deferred minor findings per task are listed in the ledger; the final whole-branch review should triage them.

## Known pre-existing issues (not from this work)

- clippy `redundant_clone` at `crates/agent_harness/src/usage.rs:171` and a warning at `crates/workspace/src/status_bar.rs:220`.
- Flaky `agent_chat` test `send_now_shows_the_message_and_queue_order_survives_idle`: fails under full-suite load, passes alone.
- Drive E: fills up: `target/debug/incremental` grows to 100+ GB and `cargo trim` does not shrink it. Delete that folder when space runs low (it is a rebuild cache).

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

## How to resume

1. In a new Claude Code session in `E:\Ale\wu`, ask to continue executing `docs/superpowers/plans/2026-11-25-claude-vscode-parity.md` from Task 33 with subagent-driven development.
2. The executor should read the ledger first (`.superpowers/sdd/2026-11-25-claude-vscode-parity/progress.md`), plus `constraints.md`, `implementer-instructions.md`, `reviewer-instructions.md` and `rereviewer-instructions.md` in the same folder.
3. Estimated remaining time at the measured pace: about 6.5-7 hours.
