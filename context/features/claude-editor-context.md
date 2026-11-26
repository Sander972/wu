---
type: feature
status: active
area: agent_chat
tags: [claude, editor-context, mentions]
---

# Claude editor context and mentions

Claude Code chats see what the user is looking at: the selected lines and open file ride along with each message, and files, selections, terminals and PDFs can be mentioned or attached from the editor and panels. Code: `crates/agent_chat/src/chat_view/editor_context.rs` (chip, mentions, Fix with Claude, terminal mentions), `crates/agent_chat/src/chat_view/completion.rs` (`@` menu), `crates/agent_chat/src/chat_view/attachments.rs` (PDFs), `crates/agent_chat/src/agent_chat.rs` (actions), `crates/project_panel` and `crates/git_ui` (Add to Chat).

## Behavior

- A footer chip shows "N lines selected" or the open file. The text is read only when sending, and dismissing the chip stops that selection from being resent. Settings: `attach_selection`, `attach_open_file` (both default true).
- Ignored and private files (`private_files`, such as `.env`) are sent path only, never their selection text. This is wu's version of the extension's `respectGitIgnore`.
- Alt+K (`agent::InsertMention`) inserts `@path#5-10` for the selection into the chat that had focus last, whether it is a tab or docked. With a terminal focused it goes to the connected terminal `claude` instead, see [[claude-ide-server]]. A path with whitespace or `:` is quoted, and one containing `"` or `#` gets no line suffix.
- "Add to Chat" in the project panel and Git panel context menus mentions the selected files.
- "Fix with Claude" is a code action on errors and warnings (`show_fix_actions`, default true, local projects only). It fills the composer with `Fix this problem in @file#L1-3 ` and sends nothing until Enter. The diagnostics block is attached only while that mention is still in the text.
- "Resolve Merge Conflicts" fills the composer with a prompt listing exactly the conflicted files, separated from whatever was already typed.
- `@terminal:name` attaches that terminal's output. Terminals with the same title get distinct names and punctuation after the mention is fine.
- PDFs are sent to Claude as documents (detected by extension only). Other agents get a file reference.

## Limits

- Alt+K shadows Option+K ("˚") in editors on macOS, as the plan required.
- Mention numbers for same-titled terminals shift when an earlier one closes.
- A retried undelivered message loses its context, and a queued message's context is not shown.
