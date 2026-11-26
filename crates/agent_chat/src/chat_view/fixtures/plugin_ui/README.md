# Claude plugin drawing fixtures

Raw answers of the Claude Code CLI to the plugin drawing ("mods") requests, recorded for the Phase 10 rendering code. Every `.json` file except `init-capabilities.json` is one `control_response` line exactly as the CLI wrote it, pretty-printed.

## Recording

- Claude Code CLI 2.1.332 (`claude --version`), Windows, first-party claude.ai login. VS Code extension used for code reading: `anthropic.claude-code-2.1.332-win32-x64`.
- Recorded 2026-11-26 with a scratch script (not in the repo) that ran `claude --print --input-format stream-json --output-format stream-json --verbose --no-session-persistence --plugin-dir <scratch>/wu-draw-test` and sent, in order: `initialize`, `ui_attach` (`surface: "wu"`, then `surface: "vscode"`), `ui_render` for `ToolUse`, `ui_render` for `Pane`, `ui_panes`, then one user message ("Reply with the single word ok.") to get the `system init` frame.
- `client_id` was `0f6c2b8e-3d1a-4c55-9a7e-2b1f4e6d8a90`, viewport `{"columns":100,"rows":40,"isFullscreen":false}`, `answers` `["ui_copy","ui_prompt_read","ui_prompt_fill","ui_prompt_suggest"]`, no `draws`.
- The `ToolUse` render used synthetic props (no real tool call): `{"tool_use_id":"toolu_wu_draw_test_1","tool":"Bash","input":{"command":"echo hi","description":"Print hi"},"isRunning":false,"isErrored":false,"isInterrupted":false,"output":{"stdout":"hi\n","stderr":"","interrupted":false}}`, `instance_id` = the `tool_use_id`. The `Pane` render used `instance_id: "wu-draw-test-pane"` (the pane id) and `props: {}`.
- `press.handle` numbers are per process and change on every run.
- `init-capabilities.json` is the `system init` frame cut down to `type`, `subtype` and `capabilities` (the rest of that frame carries local paths).

## Plugin used

`wu-draw-test` 0.1.0, test equipment kept outside the repo and loaded only through `--plugin-dir` (nothing was written under `~/.claude`). `.claude-plugin/plugin.json` is `{ "name": "wu-draw-test", "version": "0.1.0", "description": "Test equipment for recording the plugin drawing protocol" }`, `hooks/hooks.json` is `{ "modules": ["./register.tsx"] }`, and `hooks/register.tsx` is:

```tsx
import type { Register } from 'claude-code'

const PANE = 'wu-draw-test-pane'

export const register: Register = on => {
  on('session.start', async ($, e, next) => {
    $.ui.status('wu-draw-test status')
    $.ui.toast('wu-draw-test toast')
    void $.ui.open({ id: PANE, title: 'Draw test' })

    return next(e)
  })

  on('ui.render', { component: 'ToolUse' }, async ($, e, next) => {
    const { Box, Text, Button } = $.ui.resolve(e)
    const row = await next(e)

    return (
      <Box flexDirection="column">
        {row}
        <Text dimColor>wu-draw-test line</Text>
        <Button key="ping" label="Ping" onPress={() => $.ui.toast('pressed')} />
      </Box>
    )
  })

  on('ui.render', { component: 'Pane', requestId: PANE }, async ($, e) => {
    const { Box, Text, Input, Select, Link } = $.ui.resolve(e)

    return (
      <Box flexDirection="column">
        <Text>Pane text</Text>
        <Link href="https://example.com/docs">Docs</Link>
        <Input key="name" label="Name" placeholder="type here" onSubmit={value => $.ui.toast(`submitted ${value}`)} />
        <Select key="color" label="Color" value="red" options={[{ value: 'red', label: 'Red' }, { value: 'blue' }]} onSelect={value => $.ui.status(`picked ${value}`)} />
      </Box>
    )
  })
}
```

## Capability that turns drawing on

`ui_surface_v1`. The extension (`noteCapabilities`) offers drawing when the `capabilities` array of the `system init` frame includes `"ui_surface_v1"`; the same list is also in the `initialize` control response's `capabilities`. A second, optional word, `ui_hooked_v1`, comes with entries `ui_hooked_v1:<Component>` naming the hooked components; when `ui_surface_v1` is present without `ui_hooked_v1` the extension keeps asking for every component. Neither list carried `ui_hooked_v1` in this recording.

## Surface names (decision gate)

- `surface: "wu"` is refused. Exact error: `ui_attach: surface must be "desktop", "mobile" or "vscode", client_id 1-64 of letters, digits, . _ - (the colon is the engine's), viewport (when given) positive integer columns and rows with isFullscreen (when given) a boolean, answers (when given) a list of "ui_copy", "ui_prompt_read", "ui_prompt_fill", "ui_prompt_paint", "ui_prompt_suggest", "ui_read_selection", and draws (when given) a list of at most 16 names of at most 64 characters ("Theme", "Input", "Progress", "Select" and "Shader" are the ones known; another is ignored), with later_answers (when given) a boolean`. The same rule is in the CLI binary for `ui_render`.
- `surface: "vscode"` is accepted and answers `{"surfaces":["vscode"]}` (`attach.json`).
- The user approved (2026-11-26) presenting wu as `"vscode"` for the drawing requests only, as a temporary choice. Only the `surface` field says `vscode`; `CLAUDE_CODE_ENTRYPOINT` and every other identification stay wu's own. Task 47 keeps the name in one `SURFACE` constant so it can be switched later.
- The CLI did not refuse any drawing request for being started inside a Claude Code session (`started_by_session`).
- On the `vscode` surface, `Input` and `Select` arrive without naming them in `draws` (the `draws` gate applies to mobile clients).

## Tree grammar observed

A `ui_render` answer is `{tree, props, rewritten, hooked, drawn_by}`:

- `tree`: the node tree below.
- `props`: the component props the engine draws with (here the props that were sent).
- `rewritten`: `true` for `ToolUse`, `false` for `Pane` in this recording.
- `hooked`: `true` when a loaded plugin hooks `ui.render` for that component; per the CLI schema, `false` lets the host stop asking for it until the next `ui_invalidate`.
- `drawn_by`: plugin names in chain order (`["wu-draw-test"]`); an empty list means the drawing is Claude Code's own.

Nodes are objects with a `type`:

- `Box`, `Text` (the schema also allows `div`, `span`, `b`): `props` (optional, string/number/bool values, e.g. `flexDirection`, `dimColor`), `children` (optional array of nodes and plain strings). The root node of each hook's drawing carries `part: {"plugin": "<name>"}`, marking where that plugin's drawing starts.
- `Button`: `props {key, label}`, `press {plugin, handle}`. The schema also allows `hover` and `children`.
- `Input`: `props {key, label?, placeholder?, value?, submitLabel?, rows?, autoFocus?}`, `press {plugin, handle}`.
- `Select`: `props {key, label?, options[{value, label?}], value?, autoFocus?}`, `press {plugin, handle}`.
- `Link`: `props {href, label?}`, `children` (the text drawn). It has no `press`: the host opens `href` (https, or http://localhost).
- `engine`: `{"type":"engine","ref":N}`, where the host draws its own component (the `next(e)` row inside the `ToolUse` tree). Per the CLI schema, `ref: 0` draws the original props and any other ref uses the `props` of the answer. Observed `ref: 1`.
- Known from the CLI schema but not drawn here: `Code`, `Markdown` (`key?`, `text`, `dimColor?`, `pressableLinks?`, and a `press` when its plugin answers its links), `Svg`, `Client`, `Progress` (mobile only), `Shader` (backdrops only).

Where the addressing fields appear:

- `plugin` and `handle`: only in a node's `press` object (`Button`, `Input`, `Select`, a pressable `Markdown`), and in `part.plugin` on a drawing's root. `ui_press`, `ui_input` and `ui_select` send them back as they were.
- `key`: in the node's `props` (`Button`, `Input`, `Select`, optional on `Box` and `Markdown`). Sent back as `key` in `ui_press`/`ui_input`/`ui_select`.
- `href`: in a `Link`'s `props`. `ui_press` carries `href` only for a link pressed inside a `Markdown` node that has a `press`.

## System frames observed

Pushed on stdout without a request, right after start:

```json
{"type":"system","subtype":"ui_status","plugin":"wu-draw-test","text":"wu-draw-test status","uuid":"…","session_id":"…"}
{"type":"system","subtype":"ui_toast","plugin":"wu-draw-test","text":"wu-draw-test toast","timeout_ms":4000,"uuid":"…","session_id":"…"}
{"type":"system","subtype":"ui_panes","panes":[{"id":"wu-draw-test-pane","title":"Draw test","plugin":"wu-draw-test"}],"shown_id":"wu-draw-test-pane","focused_id":null,"focus_requested_id":null,"uuid":"…","session_id":"…"}
```

`ui_panes` was pushed again after the `ui_attach`. The `ui_panes` request answers the same roster (`panes.json`).
