---
type: known-issues
area: build
tags: [windows, build, linker, inno-setup]
---

# Known issues

Windows build pitfalls and their workarounds. Procedures live in [[windows-build]].

## New shell scripts lose the exec bit on Windows

- Symptom: a `.sh` created on Windows is committed as `100644` and won't run directly on Linux/macOS.
- Fix: `git add --chmod=+x <file>`, check with `git ls-files -s <file>` (`100755`). A later `git reset` on the untracked file drops the mode, so stage it last. Seen on 2026-11-25 with `build-dev.sh` / `build-stable.sh` ([[linux-build]]).

## Drive E: full stops builds and file writes

- Symptom: `failed to create directory target\debug\.fingerprint\...` with `Spazio su disco insufficiente. (os error 112)`, and editor writes fail with `ENOSPC`.
- Cause: `target/` grows unbounded across feature sets until E: has 0 bytes free.
- Fix: run `cargo trim` (user alias for `cargo sweep --maxsize 25GB`); on 2026-11-24 it freed ~67 GB. Next debug build is close to a full rebuild, see [[windows-build]].

## `Expand-Archive` extracts nothing when cargo runs under pwsh 7

- Symptom: `wu@1.5.0: Failed to copy conpty.dll ... (os error 3)` and the same for `OpenConsole.exe`. The `out/conpty/` dir exists but is empty; `conpty.nupkg.zip` is valid.
- Cause: `build-stable.ps1` runs `bundle-windows.ps1` under `pwsh` 7, and `crates/wu/build.rs` spawned Windows PowerShell 5.1 `Expand-Archive`, which exited 0 without writing files.
- Fix (2026-11-24): `crates/wu/build.rs` extracts with `tar -xf <zip> -C <dir>` (System32 bsdtar reads .zip; Rust's `Command` resolves System32 before PATH, so Git's GNU tar is not picked).
- Impact was dev-only: `bundle-windows.ps1` `DownloadConpty` fetches conpty for the installer on its own.

## LNK4217 flood when linking `wu` / `remote_server`

- `wasm_*` / `wasmtime_*` "defined in wasmtime_c_api ... imported by libtree_sitter": tree-sitter's C code declares the wasmtime C API `dllimport` while wasmtime is linked statically. Harmless.
- Silenced with `cargo:rustc-link-arg=/IGNORE:4217` in `crates/wu/build.rs` (msvc block) and `crates/remote_server/build.rs`.
- TODO: `remote_server` fix not yet verified by a rebuild.

## LNK4104 in `explorer_command_injector`

- `export of symbol 'DllGetClassObject' should be PRIVATE` comes from the rustc-generated `lib.def`. Harmless.
- Silenced with `/IGNORE:4104` in `crates/explorer_command_injector/build.rs` (verified).

## Inno Setup Chinese messages drift

- After an Inno Setup upgrade, `Default.zh-cn.isl` produces "not recognized" / "has not been defined for the simplifiedChinese language" warnings.
- Fix: replace `crates/wu/resources/windows/messages/Default.zh-cn.isl` with the official file (now official, not `Unofficial/`): `https://raw.githubusercontent.com/jrsoftware/issrc/main/Files/Languages/ChineseSimplified.isl`. It is not shipped in the local Inno install. Synced for Inno 6.7.3 on 2026-11-24.
- `[UninstallRun]` entries need `RunOnceId` or ISCC warns.

## White title bar in secondary windows with Wu Dark Opaque / Translucent

- Symptom: the settings window title bar (min/max/close row) is white while the rest follows the dark theme.
- Cause: `assets/themes/wu/wu.json` sets `title_bar.background` and `title_bar.inactive_background` to `#00000000` for "Wu Dark Opaque" and "Wu Dark Translucent". `PlatformTitleBar` paints that transparent color, so whatever is behind it shows. The main window's root paints the theme `background`; a window root that paints nothing shows the white native surface.
- Fix (2026-11-24): `.bg(cx.theme().colors().background)` on the root `v_flex` in `impl Render for SettingsWindow` (`crates/settings_ui/src/settings_ui.rs`). TODO: not verified visually in the running app.
- Rule: any new window that embeds `PlatformTitleBar` must paint `colors().background` on its root. See [[title-bar]].

## `\n` escapes turn into real newlines when inserting zh-CN schema keys via a Python heredoc

- Symptom: a line inserted into `assets/locales/zh-CN/schema/settings.json` by a `python - <<'EOF'` script (Bash tool), written with `\n` in the Python source, landed in the file with real line breaks, splitting the JSON string across lines.
- Fix: insert or repair schema keys with the Edit tool, then run `cargo test -p settings --lib test_schema_descriptions_are_translated_to_simplified_chinese`. Rules: [[constraints]].
- TODO: root cause unknown (seen once on 2026-11-24), possibly the shell/tool layer unescaping backslashes.
