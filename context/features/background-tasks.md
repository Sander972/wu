---
type: feature
status: active
area: agent_chat
tags: [background-tasks, claude, output]
---

# Background tasks indicator

The "Command running / N tasks running" chip in the agent chat opens a Background tasks card that lists live tasks. Code: `crates/agent_chat/src/chat_view/background_tasks.rs`.

## Behavior

- Subagent rows: click → `open_subagent` (tooltip "Open subagent").
- Shell rows: click → opens Claude's `.output` file in the side pane via `ChatView::open_file` (tooltip "Show output"). Added in commit `c484e29` (2026-11-23).
- The output path is parsed from the backgrounded Bash tool result text: `... Output is being written to: <path>.output. ...` (`output_file()`). Checked 2026-11-24 against real Claude transcripts: the format still matches. Example: `C:\Users\<user>\AppData\Local\Temp\claude\<hash>\<session>\tasks\<id>.output`.
- The card also tails each running shell task: it polls the CLI's `get_task_output` every 2 seconds and shows the last 4 lines (progress-bar carriage-return redraws keep their last segment). Each task row has its own Stop (`stop_task`). Added with Task 39 of the Claude parity work, see [[claude-composer-and-transcript]].

## Edge cases

- A command piped through a filter (e.g. `cargo build -p wu 2>&1 | grep -E ...`) writes only the matched lines, and grep block-buffers when stdout isn't a TTY. So the `.output` file stays empty or nearly empty until the command ends. The opened file looks broken, but it is working correctly.
- The opened file reloads when it changes on disk, but it opens at the top and does not follow the end like a terminal does.
- A Wu build older than `c484e29` has no clickable rows.
