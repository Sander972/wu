---
type: feature
status: active
area: agent_chat
tags: [claude, sessions, archive, groups]
---

# Claude session list

The agent panel's chat list can archive, group and filter chats, and chat titles are shared with the Claude Code CLI. Code: `crates/agent_chat/src/agent_panel.rs` (`ChatFilter`, groups, rows), `crates/agent_chat/src/session.rs` (`should_auto_archive`, `auto_archive_stale`, `create_group`, `move_to_group`, continue-after-reload), `crates/agent_harness/src/claude/history.rs` (titles), `crates/agent_chat/src/agent_chat.rs` (actions).

## Behavior

- Archive and "Unarchive all" move chats under an Archived group. `archive_inactive_sessions` (default 14, or 0, 1, 2, 7) archives chats untouched for that many days, checked hourly. Open, pinned, unread, grouped and running chats are kept, and so are side chats.
- Auto-archive waits 60 seconds after startup (`AUTO_ARCHIVE_STARTUP_DELAY`) so chats being restored into tabs are not archived first. The accepted worst case is an open chat listed under Archived, which can be unarchived.
- A chat saved with the legacy `archived: true` loads as archived (accepted, release-note it).
- Groups: multi-select with shift-click, "New group", rename, move chats between groups, up to 100 groups per project (`MAX_GROUPS_PER_PROJECT`). Selecting chats from several projects silently drops the other projects' chats.
- Filters: an Active toggle plus a funnel menu with Needs input, Working, Completed, Open and Closed (OR-ed), and sort, compact and "all projects" options. "Mark as unread" keeps a chat unread until the user interacts with it. Tabs show a blue dot for a pending question and an orange one for a finished unseen chat.
- Titles: renaming a chat writes a `custom-title` line to the CLI transcript, and the title is read back from `custom-title` or else `ai-title`, so `claude --resume` shows the same title. Shortcuts: `NewClaudeChat` (Ctrl+N, off by default via `enable_new_conversation_shortcut`), `ReopenClosedChat` (Ctrl+Shift+T), `FocusInput` (Ctrl+Escape, Ctrl+Alt+Escape) and `FocusLastMessage`.
- `continue_after_reload` (default true): a Claude turn cut off by quitting wu continues when the chat is opened again within 10 minutes. wu starts the CLI with `CLAUDE_CODE_RESUME_INTERRUPTED_TURN=1` and a max age, and the chat shows "Continuing…". Inherited `CLAUDE_CODE_RESUME_*` variables are stripped from normal launches.

## Limits

- Confirm manually that no orphaned `claude.exe` survives wu exit on Windows (`kill_on_drop` only). Resume makes that matter more.
- A 30 second no-turn fallback restarts with every stdout frame, so a slow CLI start is not killed.
