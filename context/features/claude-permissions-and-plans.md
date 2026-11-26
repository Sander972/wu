---
type: feature
status: active
area: agent_chat
tags: [claude, permissions, plan-mode]
---

# Claude permissions and plans

Claude Code permission cards can remember a rule, carry a deny reason and edit a Bash command, and plan mode shows a reviewable plan card. Part of the Claude VS Code parity work (2026-11). Code: `crates/agent_harness/src/claude.rs` (prompt and decision wire), `crates/agent_harness/src/proto.rs` (`PermissionPrompt`, `PermissionDecision`), `crates/agent_chat/src/chat_view/composer.rs` (cards), `crates/agent_chat/src/session.rs` (mode switching).

## Behavior

- "Always allow" offers a destination: this project (just you), this project (shared), all projects or this session. The last pick is remembered in the chat list prefs. The button only appears when the CLI sent `permission_suggestions` and did not set `suppress_always_allow_rule` or `plugin`.
- Deny takes an optional reason, sent as `DENY_REASON_PREFIX` plus the text, and the turn continues. A deny with no reason sends `DENY_STOP` with `interrupt: true`, which stops the turn (the old label-only Deny did not interrupt).
- A Bash prompt shows an editable command; the edited text goes back as `updatedInput`. "Always allow" with an edited command still stores the rule for the original command.
- Cards for agents other than Claude (`permission: None`) keep plain Allow and Deny.
- Plan mode (`PermissionMode::Plan`, `--permission-mode plan`) turns `ExitPlanMode` into a plan card with: Yes and use auto mode (or auto-accept edits when Auto is unavailable), Yes and manually approve edits, No keep planning. wu switches the mode itself before answering, and if Auto is refused it retries `acceptEdits`.
- Plan comments quote a sentence of the plan and travel only with "keep planning" as `[Re: "quote"] text` lines, so the Yes options are hidden while a comment is pending.
- `/plan <task>` starts planning, `/plan open` asks the CLI (`get_plan`) for the plan file and opens it in an editor tab.
- A `default` reported by the CLI after a plan is adopted as the chat's stored mode, so a chat that was on Auto stays on Ask afterwards (accepted).
- `agent_chat.allow_bypass_permissions` (default true) hides Full Access and falls back to Auto.

## Not done

- A persisted `"plan"` permission is not readable by older wu builds (downgrade only).
- The Auto label on the plan card follows the previous mode, not whether Auto is available as in the extension.
- If every candidate mode is refused the plan is approved and the failure is only logged.
