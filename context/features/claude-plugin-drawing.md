---
type: feature
status: active
area: agent_chat
tags: [claude, plugins, mods, ui]
---

# Claude plugin drawing

Claude Code plugins ("mods") can draw into the chat: decorated tool rows, a band above the prompt, toasts, a status line and side-pane tabs. wu attaches as a drawing surface and renders the component trees the plugins return. Code: `crates/agent_chat/src/chat_view/plugin_ui.rs` (protocol state, tree model, rendering), `crates/agent_chat/src/chat_view/side_pane.rs` (`PluginPanes`), `crates/agent_harness/src/claude/wire.rs` (`ui_*` frames). Recorded CLI answers and the test plugin are described in `crates/agent_chat/src/chat_view/fixtures/plugin_ui/README.md`.

## Behavior

- A run sends `ui_attach` with capability `ui_surface_v1` and answers `ui_copy`, `ui_prompt_read`, `ui_prompt_fill` and `ui_prompt_suggest`. The CLI then pushes `ui_toast`, `ui_status`, `ui_invalidate` and `ui_panes`, and wu asks for drawings with `ui_render`. Clicks, edits and selections go back as `ui_press`, `ui_input` and `ui_select`.
- A drawn row or band carries the caption "From the <plugin> plugin" (or "From the A and B plugins"). A row is re-asked when its props change, and a hooked agent row every 5 seconds since its drawing can show time passing.
- The band uses the extension's `above-prompt` instance (up to 12 rows), the spinner uses `main`, toasts are capped at 5 with a 4 second default, and plugin panes (up to 16) are tabs in the chat side pane. A pane takes focus only when its plugin asks, and Escape closes only panes with `close_on_escape`. Panes drop with the run.
- Links in a drawing open only when the href is `http` or `https` with no whitespace or control characters, and without a confirm prompt (wu's reply links have none, and plugins already run code in the CLI). The tooltip shows the URL.
- Trees tolerate unknown fields and node types and are cut at depth 64 and 5,000 nodes. Tool output given to plugins is cut at 64 Ki characters like the extension. Press handles are JSON integers (`u64`).

## Decision

- `SURFACE` in `plugin_ui.rs` is `"vscode"`: the CLI accepts only the `desktop`, `mobile` and `vscode` surfaces, and the user approved this temporarily (2026-11-26). It is kept in that one constant. `CLAUDE_CODE_ENTRYPOINT` is not set and wu does not otherwise present itself as the VS Code extension. Revisit when the CLI accepts a wu surface.

## Limits

- A docked chat in a closed panel never detaches from the drawing.
- The `ToolUse` props shape comes from a recording with synthetic props, not a real tool call.
- Only `Box`/`div`, `Text`/`span`/`b`, `Button`, `Link`, `Input`, `Select` and `engine` (nested drawing) nodes render. `Code`, `Markdown`, `Svg` and `Client` nodes are tolerated but not drawn, and the fill replace and insert branches are untested.
- The recordings used a scratch plugin (`wu-draw-test`) kept outside the repo.
