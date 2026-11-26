---
type: guide
area: build
tags: [linux, build, bundle, release-channel]
---

# Linux build

How to build the Linux tarball for a given channel. Windows equivalents: [[windows-build]]. CI rules: [[constraints]].

## Wrappers

- `./build-dev.sh` / `./build-stable.sh` (repo root) run `script/bundle-linux --channel dev|stable "$@"`, then rename `target/release/wu-linux-<arch>.tar.gz` to `wu-linux-<arch>-<channel>.tar.gz`. They mirror `build-dev.ps1` / `build-stable.ps1`.
- The rename happens in the wrapper, not in `script/bundle-linux`, because `release.yml` uploads the unsuffixed name.
- Only the tarball gets the suffix; `target/wu-remote-server-linux-<arch>.gz` keeps its name (same as Windows).
- TODO: not yet run on a Linux machine (added 2026-11-25, checked only with `bash -n`).

## `script/bundle-linux` options

- `--channel dev|stable`: lowercased, anything else exits 1; default is `crates/wu/RELEASE_CHANNEL`. Exports `ZED_RELEASE_CHANNEL` and `RELEASE_CHANNEL`.
- `--remote-server`: also builds, strips, libssl-checks and gzips `remote_server`. Off by default since 2026-11-25; the IDE app (`wu` + `cli`) is always built.
- `--flatpak`: sets `ZED_BUNDLE_TYPE=flatpak`.

## Why `--channel` works without editing `RELEASE_CHANNEL`

- `crates/release_channel/build.rs` emits `cfg(__do_not_set_zed_release_channel)` when `ZED_RELEASE_CHANNEL` is set at compile time, and `compile_time_release_channel_name()` then reads that env var instead of `include_str!("../../wu/RELEASE_CHANNEL")`. So exporting it before `cargo build` bakes the channel in, and the tracked file stays untouched.
