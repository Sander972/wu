---
type: guide
area: build
tags: [windows, build, inno-setup]
---

# Windows build: fast checks

Ways to verify build/installer changes without a full `build-stable.ps1` run (~35 min). Pitfalls: [[known-issues]].

## Linker flags go in `build.rs`, not `.cargo/config.toml`

- Changing `rustflags` invalidates every crate (full ~18 min rebuild). A `cargo:rustc-link-arg=...` in the binary's `build.rs` only re-runs that build script and relinks.
- In build scripts, check the target with `std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc")`; `cfg!(...)` there describes the host.

## Debug build to preview UI changes

```sh
cargo build -p wu
./target/debug/wu.exe .
```

- ~11 min from a mostly empty cache (e.g. right after `cargo trim`).
- The debug process shows as lowercase `wu.exe` in `tasklist`, the installed app as `Wu.exe`.
- `cargo run -p wu` works too (~3-4 min incremental). `Error: could not find zed-cli from any of: bin/wu.exe, ./cli.exe` is harmless in debug builds (fatal only with `ZED_BUNDLE`). The dev window can open behind the installed app.
- The debug exe is locked while it runs: close the dev window before rebuilding.

## Release channel

- `script/bundle-windows.ps1 -Channel dev|stable` (`-c`); without it the channel comes from `crates/wu/RELEASE_CHANNEL`.
- The parameter has `[ValidateSet('dev', 'stable')]` and is lowercased: `ValidateSet` ignores case, but `ReleaseChannel::from_str` only accepts lowercase, and a capitalized value builds a `wu.exe` that panics at startup.
- `remote_server` (`target/wu-remote-server-windows-<arch>.gz`) is built only with `-RemoteServer` (`-r`); `build-dev.ps1`/`build-stable.ps1` forward it. Same opt-in on `script/bundle-linux --remote-server` ([[linux-build]]) and `script/bundle-mac -r`; `release.yml` passes them.

## Syntax-check bundle scripts without building

```sh
bash -n script/bundle-linux && bash -n script/bundle-mac
pwsh -NoProfile -Command '$t=$null;$e=$null; [System.Management.Automation.Language.Parser]::ParseFile("script/bundle-windows.ps1",[ref]$t,[ref]$e) | Out-Null; $e.Count'
```

The pwsh one prints the parse error count (`0` = OK); `$t`/`$e` must be initialized or `[ref]` fails.

## Rebuild one package with the bundle's cache

Same flags as `script/bundle-windows.ps1`, so the existing `target/x86_64-pc-windows-msvc/release` cache is reused:

```sh
cargo --config .cargo/bundle-config.toml build --release --package wu --target x86_64-pc-windows-msvc
cargo --config .cargo/bundle-config.toml build --release --features stable --no-default-features --package explorer_command_injector --target x86_64-pc-windows-msvc
cargo --config .cargo/bundle-config.toml build --release --package remote_server --target x86_64-pc-windows-msvc
```

## Recompile the installer only

Reuses the staged `inno/x86_64/` from a previous bundle. Copy the edited `zed.iss` / `messages/*.isl` there first. From Git Bash, `MSYS_NO_PATHCONV=1` is required or the `/d...` args get mangled ("You may not specify more than one script filename"):

```sh
cp crates/wu/resources/windows/zed.iss inno/x86_64/zed.iss
cp crates/wu/resources/windows/messages/*.isl inno/x86_64/messages/
MSYS_NO_PATHCONV=1 "/c/Program Files (x86)/Inno Setup 6/ISCC.exe" 'E:\Ale\wu\inno\x86_64\zed.iss' \
  '/dAppId={{9BDBDF5C-B3B7-4F80-B831-98CFF7361C8A}' /dAppIconName=app-icon '/dShellNameShort=W&u' \
  "/dOutputDir=$(cygpath -w "$TEMP/isccout")" /dAppxFullName=Farshed.Wu_1.0.0.0_neutral__japxn1gcva8rg \
  /dAppDisplayName=Wu /dAppExeName=Wu /dAppName=Wu /dAppUserId=Farshed.Wu /dAppSetupName=Wu-x86_64 \
  /dAppMutex=Wu-Editor-Stable-Instance-Mutex '/dSourceDir=E:\Ale\wu' /dRegValueName=Wu \
  /dOldAppId=2DB0DA96-CA55-49BB-AF4F-64AF36A86712 '/dResourcesDir=E:\Ale\wu\inno\x86_64' /dVersion=1.5.0
```

Write to a temp `OutputDir` so the real `target/Wu-x86_64.exe` is not overwritten.
