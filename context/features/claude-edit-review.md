---
type: feature
status: active
area: agent_chat
tags: [claude, edit-review, diff, autosave]
---

# Claude edit review and autosave

In Ask mode a proposed `Edit` or `Write` opens in a side-by-side diff tab where the user accepts, rejects or hand-edits it before Claude writes anything. Code: `crates/agent_chat/src/chat_view/edit_review.rs` (`ProposedEditReview`, `proposed_text`, `review_decision`), opened from `composer.rs` (`review_in_editor`) and `chat_view.rs` (auto-open). Autosave: `session.rs` `save_before_file_tool` plus `PRESAVE_CALLBACK_ID` in `crates/agent_harness/src/claude/wire.rs`.

## Behavior

- A new `Edit` or `Write` prompt opens its review automatically without taking focus, so typing in the composer cannot become a hidden hand-edit. When the chat shares the pane the diff opens as a background tab. "Review in editor" on the card opens it with focus.
- Keys in the review tab: Ctrl/Cmd+Enter accepts all, Ctrl/Cmd+Shift+Backspace rejects all, Alt+Y / Alt+N accept or reject the change at the cursor (macOS: Cmd+Alt+Y / Cmd+Alt+Z). Hunk decisions are keyed on the whole original range.
- Accept sends the buffer as `updatedInput` when it differs from the proposal. Rejecting every hunk denies with `interrupt: true`.
- If the file changed on disk while the review was open, the edit is denied (no interrupt) with a "read it again" message. Original and disk text go through the same line-ending handling, so CRLF files do not look changed.
- `autosave` (default true) saves a dirty open buffer before Claude reads or edits it, through a `PreToolUse` hook on `Edit|Write|MultiEdit|Read` that the harness registers in `initialize`. A save that outlives the CLI's wait is cancelled, so it can never overwrite Claude's own write.
- The same review tab serves terminal `claude` through the IDE server `openDiff` tool, see [[claude-ide-server]].

## Limits

- Only `Edit` and `Write` are reviewable. More than 100 hunks render without per-hunk buttons.
- Accepted hunks show no visual change; the file on disk is the proof.
- Paths that differ in case or use a `\?\` prefix find no open buffer and skip the autosave silently.
- The hook was only run against the fake CLI in tests; confirm it against the real 2.1.332 CLI.
