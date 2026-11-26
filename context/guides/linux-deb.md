---
type: guide
area: build
tags: [linux, build, deb, ubuntu, packaging]
---

# Linux: .deb package for Ubuntu/Debian

`script/bundle-deb` wraps the release tarball from `script/bundle-linux` into a `.deb`. Pitfalls: [[known-issues]].

## Prerequisites (once)

```sh
script/linux                          # build dependencies (clang, lld, musl-tools, ...)
sudo apt install dpkg-dev fakeroot    # dpkg-shlibdeps, dpkg-deb
rustup target add x86_64-unknown-linux-musl   # bundle-linux also does this; needed for remote_server
```

`script/generate-licenses` installs `cargo-about` on the first run.

## Build

```sh
script/bundle-deb                     # channel from crates/wu/RELEASE_CHANNEL
script/bundle-deb --channel stable    # 'wu' package
```

- Output: `target/release/<package>_<version>_<distro>_<arch>.deb` (e.g. `wu-dev_1.5.0~dev.20261125103000.g53764b7-1_ubuntu26.04_amd64.deb`). `<distro>` is `ID` + `VERSION_ID` from the build system's `/etc/os-release`: the oldest release the package installs on. It is only in the file name, not in the package version.
- Runs a full release build (`bundle-linux`), so the first run is long.
- `--skip-build` repackages the existing `target/release/wu-linux-<arch>.tar.gz` without recompiling. Only use it when that tarball was built by `bundle-deb` (see auto-update below).
- `--revision <n>` bumps the Debian revision; `--maintainer "Name <mail>"` overrides the default taken from `git config`.

## Channel, package name and version

`--channel dev|stable` picks the channel (case-insensitive); without it the channel comes from `crates/wu/RELEASE_CHANNEL`. `bundle-deb` passes it to `bundle-linux --channel`, which exports `ZED_RELEASE_CHANNEL` (compiled into the binaries in place of the `RELEASE_CHANNEL` file, see `crates/release_channel/build.rs`) and `RELEASE_CHANNEL` (app icon, `crates/wu/build.rs`), as `bundle-windows.ps1 -Channel` does. No need to edit `crates/wu/RELEASE_CHANNEL`.

| Channel | Package | Version | Install dir |
|---|---|---|---|
| `stable` | `wu` | `<crate version>-<rev>` | `/opt/wu` |
| other (e.g. `dev`) | `wu-<channel>` | `<crate version>~<channel>.<commit date>.g<commit>-<rev>` | `/opt/wu-<channel>` |

- With `--skip-build` the channel must match the existing tarball (`wu.app` for stable, `wu-dev.app` for dev), otherwise `bundle-deb` stops.
- The commit date in dev versions makes each new commit an upgrade for dpkg; rebuilding the same commit needs `--revision 2` (or `apt install --reinstall`).
- `wu` and `wu-dev` both ship `/usr/bin/wu`, so they `Conflict`/`Replace` each other.

## Package layout

- `/opt/<package>/bin/wu` (CLI), `/opt/<package>/libexec/wu-editor`, `/opt/<package>/lib/` (bundled libs, found via `RPATH=$ORIGIN/../lib`).
- `/usr/bin/wu` is a symlink to the CLI: the CLI resolves `current_exe()` through the symlink and finds `../libexec/wu-editor`.
- `.desktop` file and icons go to `/usr/share/applications` and `/usr/share/icons/hicolor`; dpkg triggers refresh the menu and icon caches, so there are no maintainer scripts.
- `Depends` comes from `dpkg-shlibdeps` on the binaries, plus the libraries the editor loads with `dlopen` (`libvulkan1`, `libegl1`, `libwayland-client0`, `libwayland-egl1`). Check that list with `grep -ao "lib[A-Za-z0-9_+-]*\.so\.[0-9]*" target/x86_64-unknown-linux-gnu/release/wu | sort -u` after dependency changes. `mesa-vulkan-drivers` is a `Recommends`.
- Files are normalized to 0644/0755: the tarball otherwise carries the builder's umask.

## Auto-update is disabled

`bundle-deb` exports `ZED_UPDATE_EXPLANATION` before calling `bundle-linux`. The value is compiled in (`option_env!` in `crates/auto_update`, `crates/cli/build.rs`), so the built-in updater stays off and never overwrites files owned by dpkg. Updates go through a newer `.deb`. A tarball built by plain `bundle-linux` has the updater on: don't package it with `--skip-build`.

## Install, inspect, remove

```sh
sudo apt install ./target/release/wu-dev_*_amd64.deb   # resolves Depends, unlike dpkg -i
dpkg-deb --info  target/release/wu-dev_*_amd64.deb
dpkg-deb --contents target/release/wu-dev_*_amd64.deb
sudo apt remove wu-dev
```

User data (`~/.config/wu`, `~/.local/share/wu`) is not touched by remove/purge.

## Target distro compatibility

The binary links against the build machine's glibc: build on the oldest Ubuntu release you need to install on (or in a container of it). `libstdc++` is bundled; glibc cannot be.

- Built on Ubuntu 26.04 (2026-11-25) the package gets `Depends: libc6 (>= 2.43), libasound2t64`: apt refuses it on 24.04 (glibc 2.39) and older.
- For 24.04 targets use `script/bundle-deb-container` (below).

## Build for an older Ubuntu (container)

```sh
script/bundle-deb-container                       # Ubuntu 24.04 (default)
script/bundle-deb-container --ubuntu 22.04        # another release
script/bundle-deb-container --channel stable      # 'wu' package
script/bundle-deb-container -- --revision 2       # options after -- go to bundle-deb
```

- Runs `script/bundle-deb` in an `ubuntu:<version>` container (docker, or podman if docker is missing; `CONTAINER_ENGINE` overrides). The resulting `.deb` installs on that release and newer ones.
- Builder image `wu-deb-builder:ubuntu-<version>`: `script/linux` deps, `dpkg-dev`, `fakeroot` and the toolchain of `rust-toolchain.toml`, built once. After changing `script/linux` or `rust-toolchain.toml` pass `--rebuild-image`.
- Separate target dir `target/ubuntu-<version>/`: output in `target/ubuntu-<version>/release/`. Objects built against another glibc must not mix with the host's `target/`.
- `target/ubuntu-<version>/container-cache/` keeps `CARGO_HOME` (registry, `cargo-about`) between runs.
- Runs as the host user (`--user` for docker, `--userns=keep-id` for podman), so the files in `target/` are not owned by root. `Maintainer` defaults to the host's `git config`.
