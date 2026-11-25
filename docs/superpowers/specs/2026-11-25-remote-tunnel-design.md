# Remote Tunnel: open your own machines from anywhere via GitHub sign-in

Date: 2026-11-25
Status: draft, awaiting approval

## Intent

A wu user signs into wu with their GitHub account on every PC or server they own. From any of those machines they see the list of their other machines and open a remote project on one with a click: no IP address, no port forwarding, no SSH setup. This ships to every wu user, each of whom only ever sees and reaches their own machines.

The author of wu must not operate any infrastructure for this to work. Users depend only on GitHub (accounts and the machine list) and on n0's free public iroh relays (connection setup and fallback traffic), and any user can swap the relay for their own.

## Goals

- Sign in with GitHub from the wu GUI and from a headless server (no browser on the server).
- A machine list in the Remote Projects UI showing the user's other machines and whether each is reachable right now.
- Opening a remote project on a listed machine gives the same experience as an SSH remote: file editing, search, LSP, git, terminals, tasks.
- Works across NAT without configuration: direct peer-to-peer when possible, relayed otherwise.
- A setting to use a custom iroh relay instead of n0's public relays.
- Only machines registered under the same GitHub account can connect to a host.

## Non-goals (v1)

- Sharing a machine with other GitHub users or organizations.
- Installing the host side as a system service (systemd unit, Windows service, launchd). v1 documents how to run it; it does not install it.
- Automatically updating the host's wu to match the client's version (v1 detects the mismatch and tells the user).
- Debugger port forwarding and `build_forward_ports_command` over the tunnel (see Scope decisions).
- Any wu-operated backend, relay, or telemetry for this feature.

## Anti-goals

- A machine accepting a connection from anyone but its owner, for any reason.
- The GitHub token leaving the machine it was issued on.
- Breaking or changing behavior of the existing SSH and WSL remotes.

## Constraints and facts from the codebase

- The remote stack is transport-agnostic: `crates/remote/src/remote_client.rs:1570` defines `trait RemoteConnection`; SSH (`transport/ssh.rs`), WSL (`transport/wsl.rs`) and mock implement it. Messages are length-prefixed protobuf `Envelope`s (`crates/remote/src/protocol.rs`). `RemoteConnectionOptions` (`remote_client.rs:1314`) is the enum of transports and is persisted in the workspace DB (`crates/workspace/src/persistence.rs`, `RemoteConnectionKind`).
- On the host, `remote_server proxy --identifier <id> [--reconnect]` bridges its stdio to a long-lived `remote_server run` daemon via Unix sockets in the server dir, starting the daemon if needed (`crates/remote_server/src/server.rs`). The tunnel host reuses this unchanged.
- `RemoteConnection::build_command` returns a command template that is executed LOCALLY: for SSH it is `ssh -t host <cmd>`. Terminals (`crates/project/src/terminals.rs:622`), tasks, and debug adapters (`crates/project/src/debugger/dap_store.rs:328`) depend on it. A tunnel has no `ssh`, so it needs its own local helper (see Design, Terminals).
- This fork has no account sign-in (upstream Zed's was removed from `crates/client`). GitHub login is built from scratch.
- gpui exposes the OS keychain: `cx.write_credentials / read_credentials / delete_credentials` (`crates/gpui/src/app.rs:1475`).
- `tokio`, `quinn`, `rustls` are already in the dependency tree and `crates/gpui_tokio` exists, which is what iroh needs.
- Client-side binary download exists: `download_server_binary_locally` / `get_download_url` (`remote_client.rs:142-155`).
- iroh's public relays are documented by n0 as development/hobby infrastructure with no SLA and unpublished rate limits. Accepted for v1 because of the custom-relay setting.

## Approaches considered

1. **iroh + GitHub-only accounts (chosen).** GitHub device-flow login, the machine list stored in a secret gist in the user's own account, iroh QUIC connections with hole punching and n0 relay fallback. Nobody but GitHub and n0 is involved, and the relay is swappable. Trade-off: no pushed live presence; reachability is checked by pinging on demand.
2. **iroh + Convex (rejected).** Live presence and a familiar GitHub-login setup, but every user would depend on the wu author's Convex project and its quota. Rejected because the user does not want people depending on them.
3. **WebRTC + Supabase signaling (rejected).** No free TURN relay means some users can never connect; the Rust WebRTC stack is heavy.

Tailscale was noted as a zero-code alternative for personal use but is not a built-in feature.

## Design

### Components

- **`crates/remote_tunnel` (new crate, `[lib] path = "src/remote_tunnel.rs"`)**: GitHub device-flow auth, gist-backed device registry, iroh endpoint setup, the wire handshake, and the access check. Used by both the GUI and `remote_server`.
- **`crates/remote/src/transport/tunnel.rs`**: `TunnelRemoteConnection` implementing `RemoteConnection`, plus `RemoteConnectionOptions::Tunnel(TunnelConnectionOptions)`.
- **`remote_server` subcommands**: `login`, `logout`, `tunnel`, `tunnel-exec`.
- **UI in `crates/recent_projects`**: sign-in/out, "Your machines" section, "Allow remote access to this machine" toggle.
- **Settings**: a `remote_tunnel` settings section.

### Identity and sign-in

- One GitHub OAuth App owned by the wu project; only its client ID is compiled into wu (device flow needs no secret). Scope requested: `gist` (the user's GitHub username and id come from `GET /user`, which needs no extra scope).
- GitHub device flow everywhere (user decision): wu requests a device code and polls at the interval GitHub returns, honoring `slow_down`. GUI: a dialog shows the user code with a "Copy" button and opens `https://github.com/login/device` in the browser. Headless `remote_server login`: prints the code and the URL so the user can open it on any device. The user's GitHub 2FA is enforced by GitHub's own sign-in page; nothing to build for it.
- Token storage: GUI uses the OS keychain via `cx.write_credentials`. Headless `remote_server` stores it in a file in the server dir with owner-only permissions (0600 on Unix, user-only ACL on Windows). Handle a token with reduced scopes or a revoked token by signing out and prompting again.
- Each wu install has a persistent iroh secret key (Ed25519), generated once and stored next to the token (keychain for GUI, 0600 file for headless). Its public key is the machine's node ID.
- **One live endpoint per key (my call, from the builder check).** iroh relays and peers key connections by node ID, so two processes running the same key would knock each other off. Therefore exactly one process per machine owns the key and the endpoint: the GUI when wu's GUI is running (it both dials out and, when remote access is on, hosts in-process), or `remote_server tunnel` on a headless server. `tunnel-exec` never holds the key; it asks the owning process over a local socket to open a stream for it. Builder must verify the one-connection-per-node-ID behavior against the chosen iroh version.

### Device registry (secret gist)

- One secret gist per user, identified by a fixed description (e.g. `wu-remote-tunnel devices`), found via `GET /gists` and created on first sign-in if missing.
- One file per device: `device-<node_id>.json`. Each machine only ever PATCHes its own file, so concurrent writes from different machines never conflict. The one exception is "Remove this machine", which deletes another device's file.
- File contents: `{ "version": 1, "node_id", "name" (hostname, user-editable), "platform" (os+arch), "wu_version", "relay_url", "direct_addresses", "accepts_connections" (bool), "updated_at" }`.
- Written on sign-in, when "allow remote access" is toggled, when the host's relay or direct addresses change, and on rename. Deleted on sign-out. No periodic heartbeat.
- The node ID is a public key; a secret gist is unlisted but readable by anyone with its URL, which is acceptable because it contains no secrets. Writing requires the owner's token.

### Connection and transport

- iroh `Endpoint` with the device's secret key, relay mode = n0 default or the `relay_url` setting, and n0 DNS/pkarr discovery disabled: the gist is the discovery mechanism (node ID + relay URL + direct addresses).
- ALPN: `wu/remote-tunnel/1`.
- The client dials the host's node ID. iroh's TLS authenticates both sides' node IDs cryptographically.
- **Access check (host side), two layers (user decision):**
  1. **Same GitHub account.** The host re-fetches its owner's gist on every new incoming connection (one GitHub API call per connect) and requires the peer's node ID to have a `device-<node_id>.json` there. The GitHub token never leaves the machine. Known accepted risk: any other app the user authorized with `gist` scope could write a device file; layer 2 closes that gap when enabled.
  2. **Authenticator code on first connection (optional, enabled by default).** Each host has its own TOTP secret (RFC 6238, SHA-1, 6 digits, 30 s) and a local list of trusted node IDs, both stored next to its key. A peer that passes layer 1 but is not in the local trusted list gets `needs_code`; the client asks the user for the 6-digit code from their authenticator app and sends it; on success (accepting ±1 time step) the host adds the node ID to its trusted list and never asks that machine again. Failed attempts are rate-limited per node ID and globally (for example 5 failures lock that node ID out for 15 minutes) so the code can't be brute-forced. Exec streams from an already-trusted machine never prompt.
- **Revocation:** "Remove this machine" (UI on any signed-in machine, and `remote_server remove <name>`) deletes the device's gist file. Hosts drop node IDs that are no longer in the gist from their trusted list on the next gist fetch, and re-check the gist every 5 minutes while connections are open, closing connections from removed machines.
- **Handshake (first bi-directional stream):** client sends `{ protocol_version, client_wu_version, purpose: "proxy" | "exec", authenticator_code? }`. Host replies `{ ok | needs_code | wrong_code | locked_out | error, host_wu_version, platform, os_version, shell, default_system_shell }`. On version mismatch the host replies with an error and the client shows "Machine X runs wu A, this wu is B: update wu on X". The platform/shell fields replace what SSH detects with `uname`.
- **Proxy stream:** for `purpose: "proxy"` (with the `--identifier`/`--reconnect` values from `start_proxy`), the host spawns the existing `remote_server proxy --identifier <id> [--reconnect]` child process and pipes the QUIC stream to its stdin/stdout. Stderr is forwarded on a second unidirectional stream for logs. This reuses the daemon/reconnect behavior unchanged.
- **Reconnect:** if the iroh connection drops, `TunnelRemoteConnection` reports it the same way SSH does, and the existing reconnect logic in `RemoteClient` calls `start_proxy` again with `reconnect = true`.

### Host process (`remote_server tunnel`)

- Long-running headless process: loads the token and the secret key, ensures its gist entry exists with `accepts_connections: true`, binds the iroh endpoint, accepts connections, runs the access check, serves proxy and exec streams. On graceful exit it sets `accepts_connections: false`.
- On a server: the user runs `remote_server login` once, then `remote_server tunnel` (docs show a sample systemd unit; not installed by wu). On the first `tunnel` run with the authenticator enabled, it prints a terminal QR code plus the `otpauth://` URI to scan; `--reset-authenticator` generates a new secret and clears the trusted list; `--no-authenticator` turns layer 2 off.
- Authenticator setup in the GUI: turning on "Allow remote access" shows the QR code (with the URI as copyable text and a "Skip" that disables the authenticator for this machine) before the machine starts accepting connections.
- On a PC with the GUI: the "Allow remote access to this machine" toggle (persisted setting) makes the GUI's own endpoint accept incoming connections for as long as the GUI runs; for each accepted proxy stream the GUI spawns `remote_server proxy` locally, exactly like the headless host. The `remote_server` binary for the local platform is obtained with the existing download path (`download_server_binary_locally`) into the local server dir. The hosting logic lives in `remote_tunnel` so the GUI and `remote_server tunnel` share it.

### Terminals and tasks (`remote_server tunnel-exec`)

- `TunnelRemoteConnection::build_command` returns a template that runs the LOCAL `remote_server tunnel-exec --node <host_node_id> [--cwd ...] [--env ...] -- <program> <args>`, the same pattern as `ssh -t host <cmd>`.
- `tunnel-exec` connects to the key-owning GUI process over a local socket (path passed in an env var, protected by a per-session random token in the same env var); the GUI opens a stream to the host with `purpose: "exec"` and relays it. The host spawns the command in a PTY, and stdin/stdout are streamed both ways. Window-size changes of the local PTY are forwarded on a control stream. The exit code is returned as the process exit code.
- Local binary: the client machine's own `remote_server` binary, obtained with the same download path as the host case.

### Settings

```json
"remote_tunnel": {
  "relay_url": null,          // null = n0 public relays; otherwise an iroh relay URL
  "allow_remote_access": false,
  "require_authenticator": true,  // ask for a 6-digit authenticator code on a machine's first connection
  "device_name": null         // null = hostname
}
```

The GitHub OAuth client ID is a compile-time constant, overridable by env var for development.

### UI

- Remote Projects modal (`crates/recent_projects/src/remote_servers.rs`): a "Your machines" section above SSH servers.
  - Signed out: a "Sign in with GitHub" row, which shows the device code and opens the browser.
  - Signed in: the GitHub username, a "Sign out" action, the "Allow remote access to this machine" toggle, and one row per other device with name, platform, and a reachability dot (iroh ping on modal open, with a short timeout). Selecting a reachable row opens the same "pick a folder" flow SSH uses; if the host answers `needs_code`, a small prompt asks for the 6-digit authenticator code first. Each row has a "Remove this machine" action.
- Command palette actions: `remote_tunnel: sign in`, `remote_tunnel: sign out`, `remote_tunnel: toggle remote access`, `remote_tunnel: show authenticator QR code`.
- Recent projects list and persistence: tunnel projects are stored with a new `RemoteConnectionKind::Tunnel` (node ID + display name) and reopen like SSH ones.

### Scope decisions (my calls, flagged for review)

- Terminals and tasks are in v1 via `tunnel-exec`, because an editor remote without a terminal would not be "the same as SSH".
- Debug adapters that need port forwarding and `build_forward_ports_command` return a clear "not supported over remote tunnel yet" error in v1. Debug adapters launched as plain commands go through `tunnel-exec` and work.
- Online status is an on-demand ping, not a heartbeat, to keep GitHub writes rare and avoid secondary rate limits.

## Testing

- Unit tests in `remote_tunnel`: gist registry read/write against a fake HTTP client, access-check accept/reject, handshake version mismatch, TOTP verification against RFC 6238 test vectors, trusted-list add/remove, and the failed-attempt lockout.
- Integration test: two in-process iroh endpoints with relays disabled (direct loopback), a host serving a proxy stream to a stub process, and the client doing a full handshake plus message round trip.
- Existing `remote_editing_tests.rs` remain green; SSH/WSL untouched.
- Manual: two real machines on different networks (one behind NAT), sign in on both, open a project, edit, run LSP, open a terminal, kill the network, confirm reconnect.

## Left to the builder

- Exact iroh version and API wiring (tokio runtime via `gpui_tokio` in the GUI; a tokio runtime in `remote_server`).
- PTY crate on the host side (reuse what `crates/terminal` uses if it fits).
- TOTP and QR code crates (or a small in-house TOTP, it is a few lines on top of an HMAC-SHA1 crate already in the tree).
- Gist file schema details beyond the fields listed, and JSON layout.
- Error copy and UI polish within the existing Remote Projects modal style.
