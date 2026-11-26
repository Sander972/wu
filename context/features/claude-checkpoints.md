---
type: feature
status: active
area: agent_chat
tags: [claude, checkpoints, rewind, fork]
---

# Claude checkpoints, rewind and fork

Each user message has a checkpoint: "Rewind code to here" restores the files to how they were before that prompt, and "Fork conversation from here" opens a new chat holding the history before it. Code: `crates/agent_chat/src/session.rs` (`rewind_preview`, `rewind_code`, `fork_session_at`), `crates/agent_chat/src/chat_view.rs` (`rewind_code_to`, `fork_from`, `rewind_detail`), `crates/agent_harness/src/claude.rs` (prompt ids).

## Behavior

- wu launches Claude with `CLAUDE_CODE_ENABLE_SDK_FILE_CHECKPOINTING=true` and gives every user prompt an id (`Entry::User.prompt_id`). The id is the `user_message_id` of the CLI's `rewind_files` control request.
- Rewind runs a dry run first, shows the file list in a confirm dialog, stops a running turn, then repeats the dry run and refuses with a "changed since you confirmed" message when the file list differs. Only one rewind runs at a time and the message is resolved by id, not by index.
- With nothing to rewind the chat shows a "nothing to rewind" notice instead of the dialog. Rewinding to a message the chat no longer has is refused.
- Fork copies the entries before the chosen message into a new chat titled `<title> (2)` (the existing wu naming, shared with the panel Fork Chat) and puts the chosen prompt in the composer as a draft. "Fork and rewind" rewinds first, then forks, and still forks when there was nothing to rewind.
- `/fork [text]` and `/rewind` are native commands, see [[claude-dialogs-and-commands]].

## Limits

- After a stop, merged prompts give only the newest one an id, so earlier merged messages have no rewind point.
- Fork is Claude-only. `/fork` during a running turn drops the in-progress turn.
- `skippedLinks` in the rewind reply is ignored, and the confirm text says "1 files".
