---
type: feature
status: active
area: agent_chat
tags: [claude, ide, mcp, terminal]
---

# Claude IDE server

wu is an IDE for Claude Code: panel chats get an in-process `ide` tool, and a `claude` started in a wu terminal connects to a local WebSocket server through a lock file. Code: `crates/agent_chat/src/ide_server.rs` (server, lock file, tools, notifications), `crates/agent_chat/src/chat_view/edit_review.rs` (the `openDiff` tab), `crates/agent_harness/src/claude.rs` and `claude/wire.rs` (env and MCP bridge).

## Behavior

- Panel runs answer `mcp__ide__getDiagnostics` from the editors' stored diagnostics (`LspStore::stored_diagnostics`) through the `ide` MCP server that wu offers in `initialize`.
- The server listens on 127.0.0.1 on an OS-chosen port and writes `<claude config dir>/ide/<port>.lock` (`pid`, `workspaceFolders`, `ideName: "Wu"`, `transport: "ws"`, `authToken`) by write-then-rename. Connections must send the `x-claude-code-ide-authorization` header, compared against the token.
- Tools served to the terminal CLI: `getDiagnostics`, `openDiff`, `close_tab`, `closeAllDiffTabs`. Notifications sent: `selection_changed` (debounced, 0-based) and `at_mentioned` (Alt+K in a terminal).
- `openDiff` opens the same review tab as panel edits and answers `FILE_SAVED` with the final text, or `DIFF_REJECTED`. Rejecting every hunk sends `FILE_SAVED` with the unchanged text.
- Terminals created by wu get `CLAUDE_CODE_SSE_PORT`, so `/ide` in `claude` reports Wu.
- The server runs only while `agent_chat.enabled` is on. The lock file is removed on quit and a start-up sweep removes lock files of dead wu processes only, never another IDE's.
- Panel runs get `CLAUDE_CODE_SSE_PORT` removed, and a user environment variable cannot bring it back, so a panel chat never auto-connects to wu's own server.

## Limits

- Terminals opened before the setting is toggled keep a stale port.
- A second `openDiff` while focus is in the diff pane can cover the terminal tab, and a repeated `tab_name` opens two tabs.
- A full-project `getDiagnostics` has no size cap.
- Tests never write lock files into the real Claude config directory (fake file system check).
