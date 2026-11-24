---
type: feature
status: active
area: title_bar
tags: [title-bar, layout, windows]
---

# Title bar: single-row layout

When the application menu bar is always shown (`show_menus` true, e.g. Windows), the title bar is one row: menus on the left, project items (project name/host, worktree, branch) centered, update indicator on the right, then the window controls. Code: `crates/title_bar/src/title_bar.rs`, `impl Render for TitleBar`.

## Behavior

- Upstream rendered a second row under the platform title bar just for the project items; wu dropped it (2026-11-24) to save vertical space.
- Centering comes from two `flex_1` side columns (menu / right group) around the project items, passed to `PlatformTitleBar::set_children`, whose inner row uses `justify_between`.
- When menus are collapsed (`show_menus` false) the original layout is unchanged.

## Edge cases

- Narrow windows: the centered project items are wrapped in `h_flex().h_full().min_w_0().overflow_x_hidden()` so they clip first, instead of pushing the menus and update button under the window controls (fix 2026-11-24, after code review). TODO: not checked visually.
- The wrapper needs `.h_full()`: without it the inner `h_full()` row (with its `on_mouse_down` stop_propagation) shrinks to content height, so clicks just above/below the project buttons fall through to the platform title bar and drag/maximize the window.

## Theme transparency

- "Wu Dark Opaque" / "Wu Dark Translucent" use a transparent title bar background; the window root must paint the theme background behind `PlatformTitleBar`. See [[known-issues]] (white title bar in secondary windows).
