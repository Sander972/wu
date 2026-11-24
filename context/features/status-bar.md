---
type: feature
status: active
area: workspace
tags: [status-bar, settings, settings-ui]
---

# Status bar: item order and Git Graph button

The bottom bar's item order can be configured with `status_bar.left_items` / `status_bar.right_items`, and there's a Git Graph button. Added 2026-11-24, not committed yet at time of writing. Code: `crates/workspace/src/status_bar.rs` (`arranged_items`), `crates/settings_content/src/workspace.rs` (`StatusBarItem` enum).

## Ordering behavior

- Both lists are in visual left-to-right order. Listed items sit at the outer edge of their side, and unlisted items keep their registered side and order.
- An item listed in both lists goes left.
- Default: `"right_items": ["language_server"]`, so the LSP (⚡) button is at the far right, after the dock buttons. TODO: the user hasn't explicitly confirmed this default-for-everyone move (raised in code review 2026-11-24).
- Lists are `Vec`, so a user value replaces the default entirely (`MergeFrom for Vec` overwrites). Setting `right_items` without `language_server` moves it back to the left.
- Every `StatusItemView` must implement `status_bar_item(&self, cx) -> StatusBarItem` (required trait method). `PanelButtons` maps its dock position to `left_dock` / `right_dock` / `bottom_dock`.
- Duplicate entries in a list are skipped (first occurrence wins).
- Item names: `left_dock`, `right_dock`, `bottom_dock`, `diagnostics`, `git`, `git_graph`, `language_server`, `active_file`, `git_blame`, `activity_indicator`, `encoding`, `language`, `toolchain`, `line_endings`, `cursor_position`, `image_info`.

## Git Graph button

- `GitGraphButton` in `crates/wu/src/wu/git_status_item.rs`, registered right after `GitStatusItem` in `crates/wu/src/wu.rs`. It dispatches `git_ui::git_graph::Open`.
- Shown only when `status_bar.git_graph_button` (default true) is on and the project has an active repository. This is implemented as `StatusItemView::is_visible`, not an empty render.
- Separate from the Git panel footer button (commit `2e11420`), which is unchanged.

## Settings UI (Window & Layout page)

- Sections, in order: "Side Bar (Activity Bar)", "Top Bar (Title Bar)", "Bottom Bar (Status Bar)". The user wants the user-facing name first and the settings.json name in parentheses.
- "Project Panel Button" lives in the Side Bar section: with the activity bar on, left/right dock buttons render there (`dock.rs` returns an empty bar for non-bottom docks). Terminal and Debugger buttons stay under Bottom Bar because those panels dock at the bottom by default.
- The order lists use `.unimplemented()`, which shows an "Edit in settings.json" button. A drag-to-reorder UI was deliberately skipped.

## Rendering rules

- `StatusItemView::is_visible(&self, cx) -> bool` (default `true`): items returning false are filtered out before rendering. An item that renders an empty `h_flex()` still takes a slot and doubles the `gap_1` spacing. `GitStatusItem` implements `is_visible` too (hidden when `git_controls` is off; fixed after code review 2026-11-24). Rule: any item whose render returns an empty element when its setting is off should implement `is_visible` instead.
- The right-click "Hide Button" menu id is `{side}-item-menu-{entity_id}`, not the position index, so reordering via settings doesn't move an open menu onto another item.

## Pitfalls when adding a status bar setting

- Add the field to `StatusBarSettingsContent`, `StatusBarSettings` (`workspace_settings.rs`), `assets/settings/default.json`, **and** the struct initializer in `crates/settings/src/vscode_import.rs` (otherwise E0063).
- Add zh-CN translations, see [[constraints]].

## Edge cases

- The LSP popover at the right edge can't go off-screen: GPUI `anchored` (default `AnchoredFitMode::SwitchAnchor`) flips the anchor horizontally on overflow, then clamps to the window (`crates/gpui/src/elements/anchored.rs`).
- See also [[title-bar]].
