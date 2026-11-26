---
type: feature
status: active
area: agent_chat
tags: [claude, remote-control, cloud]
---

# Claude Remote Control and Web chats

`/remote-control` exposes a live chat on claude.ai and the Claude apps, and the Continue Saved Chat picker has a Web tab that continues a claude.ai/code session as a local copy. Code: `crates/agent_chat/src/session.rs` (`set_remote_control`), `crates/agent_chat/src/chat_view/claude_dialogs.rs` (`stop_remote_control`, startup toggle), `crates/agent_chat/src/saved_chats.rs` (Web tab), `crates/agent_harness/src/claude.rs` and `claude/cloud.rs` (cloud reads).

## Remote Control

- `/remote-control` sends the `remote_control` control request with the chat title as the name. The reply's `session_url` becomes a chip in the chat. Click the chip to stop. It needs a live run.
- The link lets anyone holding it drive the chat, so it is shown only on request and never logged.
- A failed or unanswered enable sends a best-effort `enabled: false` so a half-started bridge does not keep exposing the chat, unless the run already ended (then it would only start a one-off process).
- The "start with every session" toggle writes the CLI user setting `remoteControlAtStartup` and applies it to the live run. The CLI starts those sessions on its own, so wu shows no chip for them.

## Web tab

- The tab is offered only for an installed Claude CLI with no provider variable set (Bedrock, Vertex, Foundry, gateway and the like), since those have no claude.ai sign-in.
- Cloud sessions are listed with `list_cloud_sessions` (`user_initiated: true`), filtered to the project's `origin` repository, and only when the Web tab is first chosen. A transcript is read with `get_cloud_session_transcript` pages (at most 500, as in the extension) and opened as a local copy with its history. It is not a live cloud session.
- Reads use a separate reader process with `CLAUDE_CODE_SAFE_MODE`, `CLAUDE_CODE_CONFIG_PROBE` and `CLAUDE_CODE_DISABLE_CRON` set so no MCP server, hook or scheduled task runs, and a 60 second timeout. Any value of `CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC` turns the tab off with a message. An unsupported-subtype error hides it.
- Entries without a `parentUuid` chain are kept in order instead of only the tail.

## Limits

- Neither flow was run against a real account: the probe could not run from inside Claude Code. Check both by hand.
- Remote (SSH) projects are out of scope, see the parity plan's deferred table.
