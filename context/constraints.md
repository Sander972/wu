---
type: constraints
area: repo
tags: [i18n, formatting, workflow]
---

# Constraints

Rules that tests or the user enforce. Pitfalls and workarounds are in [[known-issues]].

## Every new user-facing string needs zh-CN text

- Settings UI strings (`n!(...)` titles, descriptions, section headers) go in `assets/locales/zh-CN/strings/settings_ui.json`. Enforced by `translation_tests::settings_ui_text_has_simplified_chinese` (`cargo test -p settings_ui --lib`).
- Settings schema doc comments (the `///` on `*SettingsContent` fields, including the `Default:` line) go in `assets/locales/zh-CN/schema/settings.json`, keyed by the exact rendered description. Enforced by `test_schema_descriptions_are_translated_to_simplified_chinese` (`cargo test -p settings --lib`), which also fails on stale keys.
- `t!(...)` strings go in the calling crate's file, `assets/locales/zh-CN/strings/<crate>.json` (for example `wu.json` for `crates/wu`). Each crate needs `use i18n::t;`.
- The files are key-sorted. Insert new keys in sorted position.
- `strings/*.json` also fail on stale keys: `simplified_chinese_strings_are_complete_and_consistent` (`cargo test -p i18n`) reports every key no `t!`/`n!` call uses. When renaming a string (e.g. "Status Bar" → "Bottom Bar (Status Bar)"), delete the old key. Contextual `tc!("setting value", "X")` keys are stored as `"setting value|X"`, separate from plain `"X"`.

## Format only the files you touched

- Run `rustfmt --edition 2024 <file>`, not `cargo fmt -p <crate>`. The tree isn't rustfmt-clean, so `cargo fmt -p` reformats unrelated files (on 2026-11-24 it rewrote `pane.rs` and most of `settings_ui/src/page_data.rs`; it happened again the same day, touching `pane.rs`, `crates/wu/build.rs`, `crates/wu/src/reliability.rs`).
- If it slips: in auto mode `git checkout -- <file>` is denied by the classifier, so tell the user the exact revert command. In bypass-permissions mode it ran fine (2026-11-24, reverted `pane.rs`, `crates/wu/build.rs`, `crates/wu/src/reliability.rs` after the user said "fix all"). Only revert a file whose whole diff is formatting (check `git diff <file>` first).

## `release.yml` needs the remote-server artifacts

- The `bundle_mac`, `bundle_linux` and `bundle_windows` jobs upload `target/wu-remote-server-<os>-<arch>.gz` with `if-no-files-found: error`. Since the bundle scripts build `remote_server` only on request, CI must keep passing `-r` / `--remote-server` / `-RemoteServer`. Details: [[linux-build]], [[windows-build]].

## Workflow preference

- After UI changes, the user wants to see them in the running debug app ("run the dev app"). Procedure: [[windows-build]].
