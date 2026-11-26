# Wu dependency upgrade: session hand-off

Nov 25, 2026 · @Ale

## Goal and status

**Status (Nov 26, 2026): done.** Every planned bump is committed on branch `dep-upgrade` (worktree `E:\Ale\wu-dep-upgrade`, 33 commits on top of `main`, not pushed). The only crates left out are the ones pinned on purpose and the ones blocked on forks. See "Session notes (Nov 25–26)" at the end. The sections below are the original session-1 hand-off, kept for history; their checklist is ticked off under "Next steps".

Session 1 stopped partway through, at the user's request. At that point nothing was committed, and the only files it had touched were `Cargo.toml` and `Cargo.lock` in `E:\Ale\wu` (branch `main`).

The user asked for two things: first, "update all the packages that don't have breaking changes and don't need code changes"; second, "make a full list of all the packages that need code changes and why". The user said STOP while the per-crate trial script was running (27 of 36 trials done), and the script was killed.

Working tree right now:

- `Cargo.lock`: 448 semver-compatible updates, with `time` held at 0.3.47. It also has whatever the interrupted `async-tungstenite` trial resolved.
- `Cargo.toml`: 18 changed version lines. 17 of them are verified major bumps (any\_vec, base64, cargo\_metadata, convert\_case, criterion, dirs, libsqlite3-sys, linkify, lru, parking\_lot, pollster, protox, resvg, usvg, shlex, take-until, zstd). The 18th, `async-tungstenite = "0.35"` at line 400, is **unverified**: it was the trial in flight when the user stopped it.
- Other modified files (`crates/agent_chat/*`, `context/*.md`, `script/bundle-*`, `.github/workflows/release.yml`, `assets/locales/...`, `crates/agent_harness/tests/fixtures/fake-claude.sh`) were **not** changed by this session. Don't revert or commit them as part of this work.

## What was done

The semver-compatible lockfile update passed `cargo check --workspace --all-targets` with zero warnings, once `time` was held back.

1. Survey: `cargo update --dry-run --verbose` found 448 compatible updates and 74 crates whose latest version is a new major. Toolchain is Rust 1.97.1 (`rust-toolchain.toml`), cargo 1.97.1. Neither `cargo-outdated` nor `cargo-edit` is installed.
2. Ran `cargo update`. That changed `Cargo.lock` only (about 1,770 lines added, 1,600 removed).
3. Ran `cargo check --workspace --all-targets`. It passed in 8 min, but `time` 0.3.47 → 0.3.55 deprecates `time::format_description::parse`, used at `crates/git_ui/src/git_graph.rs:615`. `script/clippy` runs with `--deny warnings`, so that counts as a needed code change.
4. Ran `cargo update -p time --precise 0.3.47`, which brought `time-core` and `time-macros` back too. A recheck of `git_ui`, `time_format` and `wu` came out clean.

Only Windows targets were compiled. macOS and Linux cfg code was not checked, and no clippy or test run covered the whole workspace.

## Major bumps kept (compiled with no code changes)

These 16 trials passed `cargo check --all-targets` on the listed crates, and their new versions are now in `Cargo.toml`. A pass only proves the code compiles: runtime behaviour wasn't tested, except where tests are listed.

| Crate | From → to | Checked crates | Tests run |
| --- | --- | --- | --- |
| any\_vec | 0.14 → 0.15 | search, workspace | — |
| base64 | 0.22 → 0.23 | agent\_harness, editor, markdown, project, remote, rpc | — |
| cargo\_metadata | 0.19 → 0.23 | xtask | — |
| convert\_case | 0.11 → 0.12 | editor, toolchain\_selector | — |
| async-tungstenite | 0.33 → 0.35 | full workspace (+ clippy) | — |
| dirs | 6 → 7 | agent\_chat, paths, terminal\_view, util, workspace | — |
| libsqlite3-sys | 0.30 → 0.40 | sqlez | sqlez passed |
| linkify | 0.10 → 0.12 | editor, markdown | — |
| lru | 0.16 → 0.19 | language\_core | language\_core passed |
| parking\_lot | 0.12 → 0.13 | wu (full app graph) | — |
| pollster | 0.4 → 1.0 | gpui, gpui\_wgpu, sqlez | — |
| protox | 0.9 → 0.10 | proto | — |
| resvg + usvg | 0.46 → 0.50 | agent\_chat, gpui, mermaid\_render, xtask | — |
| shlex | 1.3 → 2 | terminal\_view, util | util passed |
| take-until | 0.2 → 0.3 | util | util passed |
| zstd | 0.11 → 0.14 | rpc | rpc passed |

Things to watch: libsqlite3-sys 0.40 bundles a newer SQLite, so existing user databases should be tested. resvg/usvg 0.50 can render SVG icons and Mermaid diagrams differently. Linux and macOS users of parking\_lot and dirs weren't compiled.

## Bumps that need code changes (trial failed, already reverted)

Nine bumps definitely need code changes. Four more failed with errors that look unrelated to the bumped crate and need a baseline rerun before you trust them.

| Crate | From → to | Why it fails | First error site |
| --- | --- | --- | --- |
| time | 0.3.47 → 0.3.55 | `format_description::parse` is deprecated in favour of `parse_borrowed`; clippy `--deny warnings` fails | `crates/git_ui/src/git_graph.rs:615` |
| criterion | 0.5 → 0.8 | `criterion::black_box` is deprecated in favour of `std::hint::black_box`; clippy `--deny warnings` fails (12 call sites) | `crates/benchmarks/benches/display_map.rs`, `crates/language/benches/highlight_map.rs`, `crates/rope/benches/rope_benchmark.rs` |
| cargo\_toml | 0.21 → 1.0 | `Inheritable<T>` lost `is_some_and` | `tooling/xtask/src/tasks/package_conformity.rs:28` |
| chardetng | 0.1 → 1.0 | Detector constructor now takes 1 argument; a `bool` parameter became the `Utf8Detection` enum | `crates/language/src/file_content.rs:84`, `:86` |
| html5ever + markup5ever\_rcdom | 0.39 → 0.40 | Type inference breaks (the API became more generic) | `crates/markdown/src/html/html_minifier.rs:283`, `:351` |
| itertools | 0.14 → 0.15 | `Position` is no longer an enum with `Middle`/`Last` variants | `crates/editor/src/element.rs:4382`, `:4544` |
| sha2 | 0.10 → 0.11 | digest 0.11 output (`hybrid_array::Array`) has no `LowerHex`, so `format!("{:x}")` on hashes breaks | `crates/http_client/src/github_download.rs:154`, `:340` |
| rsa | 0.9 → 0.10 | `Pkcs1v15Encrypt` moved; the rand\_core 0.9 `RngCore` trait changed (`next_u32`/`next_u64` impl) | `crates/rpc/src/auth.rs:6`, `:120`, `:124` |
| sqlformat | 0.2 → 0.5 | `format()` takes `&FormatOptions`, which has no `Default` | `crates/sqlez/src/migrations.rs:87`, `:94` |
| sysinfo | 0.37 → 0.40 | `System::new*` now returns `Result<System, sysinfo::Error>` | `crates/system_specs/src/system_specs.rs:43`, `:85`; `crates/terminal/src/pty_info.rs:103` |

**Inconclusive. Rerun the same `cargo check -p … --all-targets` without the bump first.** These errors sit in code the bump doesn't touch. Checking several `-p` crates together probably unifies features (`test-support`) differently, so the same failure may happen without the bump too.

| Crate | From → to | Error seen | Checked crates |
| --- | --- | --- | --- |
| async\_zip | 0.0.18 → 0.0.19 | `windows::fs` not found inside the git fork `async-tar` (`src/entry.rs:609`) | util, dictation |
| cpal | 0.17 → 0.18 | Same `async-tar` error | dictation |
| etagere | 0.2 → 0.3 | `render_to_image` is not a member of `PlatformWindow` (`crates/gpui_windows/src/window.rs:1011`). That's a gpui feature mismatch, not an etagere API change | gpui, gpui\_wgpu, gpui\_windows |
| ordered-float | 2 → 5 | Non-exhaustive match on `RemoteConnectionOptions::Mock` (`crates/recent_projects/src/remote_connections.rs:399`). That's a test-support feature mismatch | editor, project\_symbols, recent\_projects |

Correction to the chat summary: etagere was reported there as an "atlas API change", but the recorded error doesn't support that.

Pinned on purpose, so left out of the trials: `ort = "=2.0.0-rc.13"`, `parakeet-rs = "=0.3.8"` and `rubato = "=0.16.2"` (in `crates/dictation/Cargo.toml`), `taffy = "=0.13.0"` (in `crates/gpui/Cargo.toml`), and `cocoa = "=0.26.0"`. `zed_extension_api` 0.1.0 and `wit-bindgen` 0.41 are tied to the extension ABI.

## Not yet tried

The trial script stopped before these. All of them are expected to need code changes; the reasons below come from the scope of each crate, not from compile errors.

| Crate(s) | From → to | Used by | Expected work |
| --- | --- | --- | --- |
| toml\_edit | 0.22 → 0.25 | xtask | Was next in the queue |
| toml | 0.8 → 1.x | extension, extension\_cli, extension\_host, grammars, language, language\_core, project, remote\_server, xtask | API reshuffle around `toml::Value`/`Table` and de/serialisation helpers |
| tree-sitter-css, -embedded-template, -jsdoc, -regex | 0.23/0.24 → 0.25 | grammars, languages, language | Must match the git-pinned `tree-sitter` rev (`Cargo.toml:688`); `.scm` queries may need node-name updates |
| futures-lite | 1.13 → 2 | 12 crates (editor, lsp, terminal, util, workspace…) | Removed and renamed combinators |
| taffy | =0.13 → 0.16 | gpui | Exact pin; layout engine API changes |
| wgpu + naga | 29 → 30 | gpui\_wgpu | GPU renderer API |
| windows, windows-core, windows-registry | 0.61 → 0.62 / 0.100 | 16 crates (gpui\_windows, cli, fs, terminal, wu…) | All move together; Windows platform layer |
| wasmtime + wasmtime-wasi + wasm-encoder + wasmparser | 48 → 51, 0.252 → 0.263 | extension, extension\_host, extension\_cli | All move together; extension host |
| accesskit (+ \_windows, \_macos, \_unix) | 0.24 → 0.25, \_windows 0.33 → 0.35 | gpui, gpui\_windows, gpui\_macos, gpui\_linux | All adapters move together |
| tracing-tracy + tracy-client | 0.11 → 0.12, 0.18 → 0.19 | ztracing (optional `tracy` feature) | Move together |

## macOS/Linux-only crates (changelog research, not compiled)

A research subagent diffed the published crate sources for each version pair. Every new version's MSRV is ≤ 1.97.1.

| Crate | From → to | Verdict | Detail |
| --- | --- | --- | --- |
| nix | 0.29 → 0.32 | Drop-in | Only `util/src/util.rs:232,253,271` uses it (`geteuid`, `getrlimit`/`setrlimit`, `Errno::EINVAL`); signatures unchanged |
| mach2 | 0.5 → 0.7 | Drop-in | Items used in `gpui_macos/src/dispatcher.rs` and `util/src/command/darwin.rs` are unchanged; cpal/wasmtime keep older copies |
| xkbcommon | 0.8 → 0.9 | Drop-in | Additive API; `Keymap::new_from_fd` now passes the trailing NUL (`wayland/client.rs:1873`) |
| cbindgen | 0.28 → 0.29.4 | Drop-in, but the generated output can change | `gpui_apple/build.rs`; diff the generated `scene.h` on a Mac |
| oo7 | 0.6 → 0.7.3 | Cargo.toml only | Adds ashpd 0.14 + zbus 6 duplicates (consider bumping ashpd); `Keyring::new` now detects sandboxes; unverified whether its futures are still `Send` |
| core-video | 0.5 → 0.6 | Small code change | `create_texture_from_image` takes `&CVImageBuffer` at `gpui_apple/src/metal_renderer.rs:1609`, `:1620` |
| core-graphics + core-text | 0.24 → 0.25, 21 → 22 | Blocked on fork | The `zed-font-kit` git fork (rev 94b0f28) is on core-graphics 0.24; a new zed-font-kit must be published |
| x11rb | 0.13 → 0.14 | Blocked on fork | The `zed-xim` git fork (rev 16f35a2) is on x11rb 0.13; types are passed straight between them |
| cocoa | =0.26.0 → 0.26.1 | Keep the pin | 0.26.1 deprecates `cocoa::appkit`/`quartzcore` (used in \~11 files), which fails clippy `--deny warnings` |

Not covered at all: the 31 git-pinned deps in `Cargo.toml` (compared against their upstream heads) and the npm packages in `website/package.json`.

## Next steps

Session 3 (Nov 26): all code-change bumps are done on `dep-upgrade` (32 commits after `528d7cb`, one per bump or move-together group). The only things left out are the deliberate pins (taffy, ort, parakeet-rs, rubato, cocoa, zed\_extension\_api, wit-bindgen) and the fork-blocked crates (core-graphics/core-text, x11rb). The full workspace check and clippy (`--release --all-targets --all-features`) are clean on Windows. The macOS/Linux-only bumps were verified with zig cross-target `cargo check` only. Remaining open items: `etw_tracing` stays on windows 0.61 because the `wprcontrol` git dep is tied to it; `extension_cli` still pulls wasmtime 48 through tree-sitter's `wasm` feature; oo7 0.7 adds duplicate ashpd and zbus versions on Linux (a possible fix is ashpd 0.14); two `gpui_wgpu` GPU tests crash in the Intel UHD 630 Vulkan driver on wgpu 30.

Done in session 2 (Nov 25), in worktree `E:\Ale\wu-dep-upgrade`, branch `dep-upgrade`, commit `528d7cb`: async-tungstenite 0.35 verified and kept; criterion reverted to 0.5 (needs code changes, see above); `cargo check --workspace --all-targets` and clippy (release, all targets, all features, `--deny warnings`) both clean. Clippy on `main` already fails on two lints that predate this work: `clippy::borrowed_box` at `crates/workspace/src/status_bar.rs:220` (from `d724df2`) and `clippy::redundant_clone` at `crates/agent_harness/src/usage.rs:172` (from `440c28a`; the clone is only needed on macOS). The clean run allowed those two lints. `cargo shear` isn't installed, so it didn't run. The main checkout still has the old uncommitted `Cargo.toml`/`Cargo.lock` edits (with criterion 0.8); discard them once `dep-upgrade` is merged.
- [x] Verify the unverified `async-tungstenite = "0.35"`: verified by a full workspace check and clippy, then kept (`528d7cb`)
- [x] Check that no `cargo` process from the killed trial is still running: none were
- [x] Run `cargo check --workspace --all-targets` and `./script/clippy` before committing: both clean (criterion 0.8 was pulled out of the first commit because of its `black_box` deprecation, then bumped with code changes in `5d5d057`)
- [x] Commit only `Cargo.toml` and `Cargo.lock`: `528d7cb`, on branch `dep-upgrade` in a separate worktree, not on `main`
- [x] Rerun the four inconclusive trials against a baseline: all four failures were feature-unification noise, and all four are bumped (`56971f1`, `c1f7f49`, `d705ef8`, `ba68a77`)
- [x] Bump nix, mach2 and xkbcommon: committed after zig cross-target `cargo check` (`7cf4b11`, `98cd152`, `6e05fe6`); still needs a real macOS/Linux CI run
- [x] Do the code-change bumps one at a time: one commit per bump or per group that has to move together; full list below

The trial harness is at `C:\Users\Utente\AppData\Local\Temp\wu-bumps\try_bumps.py`, outside the repo, and its results are in `bump_results.jsonl` next to it. Each trial edits one `Cargo.toml` line and runs `cargo check --all-targets -p <users>` (plus `cargo test` for listed crates). It keeps the change on success, and on failure restores the `Cargo.toml` and `Cargo.lock` bytes. Run it with `python try_bumps.py "<trial name>"` to repeat single trials; the remaining entries start at `toml_edit 0.25`.

## Session notes (Nov 25–26)

The work ran in a separate git worktree (`E:\Ale\wu-dep-upgrade`, branch `dep-upgrade`) so it wouldn't clash with another session in `E:\Ale\wu`. Sonnet subagents did the bumps in five sequential batches, because all of them share one target directory. Each bump was verified on its user crates with `cargo check --all-targets`, with clippy (`--release --all-targets --all-features --deny warnings`), and with tests where they existed. A final full `cargo check --workspace --all-targets` and a workspace clippy run were both clean.

### Commits

| Commit | Change | Code changes |
| --- | --- | --- |
| 528d7cb | Semver-compatible lockfile update plus 16 drop-in major bumps (including async-tungstenite 0.35) | none |
| f113a0f | time 0.3.55 | `format_description::parse` → `parse_borrowed::<1>` |
| 5d5d057 | criterion 0.8 | `std::hint::black_box` in 3 benches |
| 9427881 | cargo_toml 1.0 | `matches!(…, Inheritable::Inherited)` in xtask |
| 4831764 | chardetng 1.0 | new constructor args, same detection behaviour |
| d98af36 | itertools 0.15 | `Position` is now a struct (editor hover) |
| 03dce70 | sqlformat 0.5 | `&FormatOptions`; the `sql!` macro now puts a space before `?` (0.5 mis-parsed `=?1`) |
| 62dac00 | sysinfo 0.40 | `System::new*` and `PtyProcessInfo::new` return `Result` (12 files) |
| d7d9105 | sha2 0.11 + rsa 0.10 | `{:x}` → `hex::encode`, chunked hashing in dictation, rand_core 0.10 shim, a new SHA-256 test |
| 944f2ab | html5ever + markup5ever_rcdom 0.40 | dropped `.as_ref()` on atoms |
| 56971f1 | async_zip 0.0.19 | none |
| c1f7f49 | cpal 0.18 | config passed by value; xruns no longer end a recording |
| d705ef8 | etagere 0.3 | none |
| ba68a77 | ordered-float 5 | none |
| edb44d9 | toml_edit 0.25 | none |
| 7045ac0 | toml 1 | none |
| 38eb0b9 | futures-lite 2 | none |
| 72d66ba, 0a567e6, fa64262, 7b0f446 | tree-sitter css / embedded-template / jsdoc / regex 0.25 | none (every `.scm` query compiled against the new grammars) |
| b8d3cfd | tracing-tracy 0.12 + tracy-client 0.19 | ztracing manifest only |
| 9dd7aec | windows 0.62, windows-core 0.62, windows-registry 0.6, windows-numerics 0.3 | none; `etw_tracing` stays on 0.61 (`wprcontrol` git dep) |
| 7ed2bf0 | accesskit 0.25 (+ adapters) | `Tree::new` → `TreeInfo::new` in gpui |
| c755b1c | wgpu + naga 30 | 4 small API fixes in gpui_wgpu |
| 7ac7e04 | wasmtime 51, wasm-encoder/wasmparser 0.263 | 3 lines; extension_cli now uses tree-sitter's wasmtime 48 `Engine` |
| 7cf4b11 | nix 0.32 | none (linux cross-check) |
| 98cd152 | mach2 0.7 | none (apple cross-check) |
| 16f45d7 | fix gpui_linux | `const_mutex` → `Mutex::new` (parking_lot 0.13 from 528d7cb broke Linux) |
| 6e05fe6 | xkbcommon 0.9 | none (linux cross-check) |
| 13a5126 | core-video 0.6 | `&CVImageBuffer` in metal_renderer (apple cross-check) |
| a15c2fa | cbindgen 0.29 | none; generated `scene.h` has the same 50 definitions, only reordered |
| 17b02f8 | oo7 0.7 | none (linux cross-check) |

### Left out on purpose

- **Pinned:** taffy, ort, parakeet-rs, rubato, cocoa, zed\_extension\_api, wit-bindgen.
- **Blocked on git forks:** core-graphics/core-text (zed-font-kit) and x11rb (zed-xim).
- **windows-core / windows-registry 0.100:** not possible while `windows` 0.62 needs windows-core 0.62.

### Known issues and risks

- **Clippy on `main` already fails** on 2 lints in unrelated code: `clippy::borrowed_box` at `crates/workspace/src/status_bar.rs:220` and `clippy::redundant_clone` at `crates/agent_harness/src/usage.rs:172` (only needed on macOS). Verification allowed them; fix them separately.
- **wgpu 30:** 2 `gpui_wgpu` atlas tests crash in the Intel UHD 630 Windows Vulkan driver. They pass on NVIDIA and DX12. Windows wu doesn't use wgpu.
- **oo7 0.7:** adds duplicate ashpd (0.13/0.14) and zbus (5/6) on Linux. Bumping the workspace ashpd to 0.14 would remove one pair.
- **extension\_cli** still pulls wasmtime 48 through tree-sitter's `wasm` feature. The `wu` binary links only wasmtime 51.
- **Tests that fail or hang without any bump:** `auto_update` (hangs), the markdown `test_metadata_blocks_store_table_rows`, and sometimes extension\_host `test_extension_store_with_test_extension` (timeout).
- **Not compiled for real:** macOS/Linux code was only checked with zig cross-target `cargo check`. CI on those platforms is still needed.

### Manual testing

- **Dev run next to the installed Wu:** use `cargo run -p wu -- --user-data-dir <dir>`. A custom data dir gets its own single-instance mutex. Without one, the dev build hands off to the running installed Wu and exits. Seed `<dir>` with a copy of `%LOCALAPPDATA%\Wu` (and `%APPDATA%\Wu` as `<dir>\config`), and copy `db\0-stable` over `db\0-dev` to test migrations on real data. The first run on a copy of real data started with no migration errors.
- **Installer without remote\_server:** `script/bundle-windows-noserver.ps1` is `script/bundle-windows.ps1` without the `BuildRemoteServer` step (the installer never uses that output). Run it with pwsh 7: `pwsh -NoProfile -ExecutionPolicy Bypass -File script/bundle-windows-noserver.ps1 -Channel stable`. The output is `target\Wu-x86_64.exe`. A stable build replaces the installed Wu and migrates the real `%LOCALAPPDATA%\Wu\db`, so back it up first. The build was started once and stopped by the user before it finished.
- **Things to check by hand:**
  - Restored workspaces and existing databases.
  - Window resize, snap and maximize; IME and the clipboard.
  - SVG icons and Mermaid diagrams (resvg 0.50).
  - Editor hover popovers.
  - Dictation and microphone selection.
  - Installing and using registry extensions, including LSP extensions and slash commands.
  - The terminal.
  - Narrator/NVDA.
  - RPC auth against an older peer.

### Before merging

- Discard the old uncommitted `Cargo.toml` and `Cargo.lock` edits in `E:\Ale\wu`; this branch replaces them.
- After merging, remove the worktree (`git worktree remove ../wu-dep-upgrade`). Its `target/` holds tens of GB.
