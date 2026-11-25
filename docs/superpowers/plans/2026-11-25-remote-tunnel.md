# Remote Tunnel Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let a wu user sign in with GitHub on all their machines and open a remote project on any of them over a direct peer-to-peer connection, with no IP, port forwarding or SSH setup.

**Architecture:** A new `remote_tunnel` crate holds GitHub device-flow auth, the gist-backed device registry, the authenticator (TOTP) and trusted-device store, the wire handshake, and a gpui `TunnelService` global that owns the machine's single iroh endpoint (dialing out and, when enabled, hosting). `crates/remote` gains a `Tunnel` transport that rides on `TunnelService`; the host side pipes each accepted stream into the existing `remote_server proxy` process, so the daemon, reconnect and `HeadlessProject` are unchanged. Terminals and tasks use a local `remote_server tunnel-exec` helper (the tunnel equivalent of `ssh -t`) that asks the GUI over a local socket to open an exec stream; the host runs the command in a PTY.

**Tech Stack:** Rust, gpui, `iroh` 1.5 (QUIC + hole punching + n0 relays), `gpui_tokio`, `http_client` (GitHub REST), `ring` (HMAC-SHA1 for TOTP), `data-encoding` (base32), `qrcode` 0.15, `portable-pty` 0.9, `tokio-util` (compat), SQLite via `sqlez`.

**Spec:** `docs/superpowers/specs/2026-11-25-remote-tunnel-design.md` (read it before any task; this plan argues from it).

## Global Constraints

- Follow `AGENTS.md`: no `unwrap()`/`expect()` outside tests, no `let _ =` on fallible calls (use `?`, `.log_err()`, or explicit handling), no `mod.rs`, new crate uses `[lib] path = "src/remote_tunnel.rs"`, full-word variable names, comments only for non-obvious "why".
- Never hard-wrap prose, comments, docstrings or commit messages (user rule): one thought per line.
- Use `./script/clippy` instead of `cargo clippy`.
- Commits: local only, never push, never amend; no AI attribution lines in commit messages; conventional prefix style matching the repo (`feat(remote_tunnel): ...`).
- Existing SSH and WSL remotes must behave exactly as before (spec anti-goal).
- The GitHub token never leaves the machine it was issued on (spec anti-goal).
- One live iroh endpoint per machine key: the GUI process when the GUI runs, `remote_server tunnel` on a headless server. `tunnel-exec` never holds the key.
- iroh: `Endpoint::builder(presets::N0)` relay defaults; `relay_url` setting switches to `RelayMode::Custom`; iroh address lookup / n0 DNS / pkarr disabled (the gist is the discovery mechanism, so full `EndpointAddr` = id + relay URL + direct addresses is stored). Verify exact 1.5 API names on docs.rs before writing code; the spec says "node ID", iroh 1.x calls it `EndpointId`.
- ALPN: `wu/remote-tunnel/1`.
- GitHub OAuth: client ID compiled in as `GITHUB_CLIENT_ID`, overridable by env var `WU_GITHUB_CLIENT_ID`; scope `gist`; device flow only; every GitHub API request sends `Accept: application/vnd.github+json`, `X-GitHub-Api-Version: 2022-11-28`, `User-Agent: wu`.
- Gist: one secret gist per user with description `wu-remote-tunnel devices`; one file per device named `device-<endpoint_id>.json`; a device only PATCHes its own file, except "Remove this machine" which deletes another device's file.
- Device file JSON: `{ "version": 1, "endpoint_id", "name", "platform", "wu_version", "relay_url", "direct_addresses", "accepts_connections", "updated_at" }`.
- TOTP: RFC 6238, HMAC-SHA1, 6 digits, 30 s step, accept ±1 step; lockout after 5 failures per endpoint ID for 15 minutes; global lockout after 20 failures within 15 minutes for 15 minutes.
- Hosts re-fetch the gist on every new incoming connection and every 5 minutes while connections are open; connections from devices no longer in the gist are closed and the device is dropped from the trusted list.
- Settings section `remote_tunnel`: `relay_url: null`, `allow_remote_access: false`, `require_authenticator: true`, `device_name: null`.
- Handshake and exec control messages: length-prefixed JSON (u32 big-endian length, max 64 KiB per message; larger is a protocol error).
- `Cargo.toml` and `Cargo.lock` currently contain the user's unrelated uncommitted edits. Before Task 1, ask the user to commit or stash them; never commit their hunks with ours.

## Review Focus

1. **Phone clock and host clock disagree.** A code from an authenticator 25 s off must work; 65 s off must fail with a message that says to check the time on the host. Test in Task 1 (`verify_accepts_one_step_of_skew`, `verify_rejects_two_steps_of_skew`) and Task 7 (`wrong_code_error_mentions_clock`).
2. **GitHub unreachable or token revoked while hosting.** The host must fail closed for unknown devices, but must not lock out a trusted device because of a GitHub hiccup: use the last gist snapshot if it is under 5 minutes old, otherwise reject. A 401 from GitHub signs the host out and rejects everyone. Tests in Task 7 (`access_check_uses_recent_snapshot_when_github_fails`, `access_check_rejects_when_snapshot_is_stale`, `access_check_rejects_after_unauthorized`).
3. **Network drop in the middle of a session.** The client must reconnect through the existing `RemoteClient` reconnect path (`start_proxy` with `reconnect = true`) and the host's `remote_server run` daemon must survive. Test in Task 8 (`tunnel_reconnect_reuses_daemon`).
4. **Versions differ between client and host.** Clear error naming the host and both versions, no hang. Test in Task 8 (`version_mismatch_reports_both_versions`).
5. **Two machines with the same hostname.** The machine list must still tell them apart: show platform and the first 8 chars of the endpoint ID when names collide. Test in Task 12 (`duplicate_names_show_short_id`).

---

## Task 0 (user, before any code): Prerequisites

- [ ] **Step 1:** User registers a GitHub OAuth App (Settings → Developer settings → OAuth Apps → New), name `wu`, any homepage URL, callback URL `http://127.0.0.1` (unused by device flow), and ticks **Enable Device Flow**. Copy the Client ID.
- [ ] **Step 2:** User commits or stashes the pending `Cargo.toml` / `Cargo.lock` edits so Task 1 starts from a clean dependency state.

---

### Task 1: `remote_tunnel` crate and authenticator (TOTP)

**Files:**
- Modify: `Cargo.toml` (workspace member `crates/remote_tunnel`; `[workspace.dependencies]`: `remote_tunnel = { path = "crates/remote_tunnel" }`, `iroh = "1.5"`, `ring = "0.17"`, `data-encoding = "2"`, `qrcode = { version = "0.15", default-features = false }`)
- Create: `crates/remote_tunnel/Cargo.toml` (`[lib] path = "src/remote_tunnel.rs"`, `license = "GPL-3.0-or-later"`, `publish.workspace = true`, `[lints] workspace = true`)
- Create: `crates/remote_tunnel/src/remote_tunnel.rs` (module declarations and re-exports only)
- Create: `crates/remote_tunnel/src/authenticator.rs`

**Interfaces:**
- Produces:
  - `pub struct TotpSecret([u8; 20])` with `pub fn generate() -> Self` (ring `SystemRandom`), `pub fn from_bytes(&[u8]) -> Result<Self>`, `pub fn as_bytes(&self) -> &[u8]`, `pub fn base32(&self) -> String` (RFC 4648, no padding).
  - `pub fn totp_code(secret: &TotpSecret, unix_seconds: u64) -> String` (6 digits, zero-padded).
  - `pub fn verify_totp(secret: &TotpSecret, code: &str, unix_seconds: u64) -> bool` (accepts steps t-1, t, t+1; rejects non-6-digit input; constant-time compare via `ring::constant_time` or equivalent).
  - `pub fn otpauth_uri(secret: &TotpSecret, account: &str, issuer: &str) -> String` → `otpauth://totp/{issuer}:{account}?secret={base32}&issuer={issuer}&algorithm=SHA1&digits=6&period=30` (URL-encode label parts).
  - `pub fn qr_code_text(uri: &str) -> Result<String>` (qrcode `render::unicode::Dense1x2`, light/dark chosen so it scans on dark terminals).

- [ ] **Step 1: Write failing tests** in `authenticator.rs` (`#[cfg(test)] mod tests`), using the RFC 6238 SHA-1 seed `b"12345678901234567890"`:
  - `rfc6238_vectors`: `totp_code(&seed, 59) == "287082"`, `totp_code(&seed, 1111111109) == "081804"`, `totp_code(&seed, 1234567890) == "005924"`.
  - `verify_accepts_one_step_of_skew`: `verify_totp(&seed, &totp_code(&seed, 1000), 1000 + 30)` and `... 1000 - 30` are true.
  - `verify_rejects_two_steps_of_skew`: same with `±60` is false.
  - `verify_rejects_malformed`: `"12345"`, `"1234567"`, `"abcdef"`, `""` all false.
  - `base32_roundtrip`: `data_encoding::BASE32_NOPAD.decode(secret.base32().as_bytes()) == secret.as_bytes()`.
  - `otpauth_uri_format`: for seed, account `"Desktop"`, issuer `"wu"`, the URI starts with `otpauth://totp/wu:Desktop?secret=GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ` and contains `&digits=6&period=30`.
- [ ] **Step 2:** Run `cargo test -p remote_tunnel authenticator` → FAIL (functions not defined).
- [ ] **Step 3:** Implement the functions above (HMAC via `ring::hmac::HMAC_SHA1_FOR_LEGACY_USE_ONLY`, dynamic truncation per RFC 4226 §5.3).
- [ ] **Step 4:** Run `cargo test -p remote_tunnel authenticator` → PASS.
- [ ] **Step 5:** Commit `feat(remote_tunnel): add crate and TOTP authenticator`.

---

### Task 2: Wire protocol (handshake and framing)

**Files:**
- Create: `crates/remote_tunnel/src/protocol.rs`

**Interfaces:**
- Produces:
  - `pub const ALPN: &[u8] = b"wu/remote-tunnel/1";`
  - `pub const PROTOCOL_VERSION: u32 = 1;`
  - `pub const MAX_MESSAGE_LEN: usize = 64 * 1024;`
  - `#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)] #[serde(tag = "purpose", rename_all = "snake_case")] pub enum Purpose { Ping, Proxy { identifier: String, reconnect: bool }, Exec(ExecRequest) }`
  - `pub struct ExecRequest { pub program: Option<String>, pub args: Vec<String>, pub env: HashMap<String, String>, pub working_dir: Option<String>, pub interactive: bool, pub columns: u16, pub rows: u16 }`
  - `pub struct ClientHello { pub protocol_version: u32, pub client_wu_version: String, pub purpose: Purpose, pub authenticator_code: Option<String> }`
  - `#[serde(tag = "status", rename_all = "snake_case")] pub enum HostReply { Ok(HostInfo), NeedsCode, WrongCode { attempts_left: u32 }, LockedOut { retry_after_seconds: u64 }, VersionMismatch { host_wu_version: String }, Rejected { reason: String } }`
  - `pub struct HostInfo { pub host_wu_version: String, pub platform: PlatformInfo, pub os_version: Option<String>, pub shell: String, pub default_system_shell: String }` and `pub struct PlatformInfo { pub os: String /* "linux"|"macos"|"windows" */, pub arch: String /* "x86_64"|"aarch64" */ }`
  - `#[serde(tag = "type", rename_all = "snake_case")] pub enum ExecControl { Resize { columns: u16, rows: u16 }, Exit { code: i32 } }`
  - `pub async fn write_json<W: AsyncWrite + Unpin, T: Serialize>(writer: &mut W, value: &T) -> Result<()>` and `pub async fn read_json<R: AsyncRead + Unpin, T: DeserializeOwned>(reader: &mut R) -> Result<T>` (futures-io traits; u32 BE length prefix; reject `> MAX_MESSAGE_LEN` before allocating).

- [ ] **Step 1: Write failing tests:** `hello_roundtrip` (write then read a `ClientHello` with `Purpose::Proxy { identifier: "abc", reconnect: true }` through `futures::io::Cursor`), `reply_roundtrip_all_variants`, `rejects_oversized_message` (a prefix of `65537` returns an error without reading the body), `rejects_truncated_message`.
- [ ] **Step 2:** `cargo test -p remote_tunnel protocol` → FAIL.
- [ ] **Step 3:** Implement the types and the two functions.
- [ ] **Step 4:** `cargo test -p remote_tunnel protocol` → PASS.
- [ ] **Step 5:** Commit `feat(remote_tunnel): add handshake protocol`.

---

### Task 3: Secret storage, trusted devices and lockout

**Files:**
- Create: `crates/remote_tunnel/src/local_state.rs`

**Interfaces:**
- Produces:
  - `pub enum SecretStore { Keychain, File(PathBuf) }` with `pub async fn read(&self, name: SecretName, cx: &AsyncApp) -> Result<Option<Vec<u8>>>`, `pub async fn write(&self, name: SecretName, value: &[u8], cx: &AsyncApp) -> Result<()>`, `pub async fn delete(&self, name: SecretName, cx: &AsyncApp) -> Result<()>`. `Keychain` uses `cx.read_credentials/write_credentials/delete_credentials` with URLs `wu-remote-tunnel://github-token`, `wu-remote-tunnel://endpoint-key`, `wu-remote-tunnel://totp-secret`. `File(dir)` stores one JSON file `dir/secrets.json` written atomically (temp file + rename) with mode 0600 on Unix; on Windows rely on the per-user `%LOCALAPPDATA%` ACL (comment why).
  - `pub enum SecretName { GithubToken, EndpointKey, TotpSecret }`
  - `pub fn state_dir(profile: Profile) -> PathBuf` → `paths::data_dir().join("remote_tunnel").join(profile.as_str())`, `pub enum Profile { App, Server }` (`"app"`, `"server"`).
  - `pub struct TrustedDevices` persisted as `state_dir/trusted_devices.json` (`Vec<String>` of endpoint IDs): `pub fn load(path: &Path) -> Result<Self>` (missing file = empty), `pub fn contains(&self, endpoint_id: &str) -> bool`, `pub fn insert(&mut self, endpoint_id: String) -> Result<()>` (persists), `pub fn retain_only(&mut self, allowed: &HashSet<String>) -> Result<()>` (persists).
  - `pub struct AttemptLimiter` (in memory): `pub fn check(&self, endpoint_id: &str, now: Instant) -> Result<(), Duration /* retry after */>`, `pub fn record_failure(&mut self, endpoint_id: &str, now: Instant) -> u32 /* attempts left */`, `pub fn record_success(&mut self, endpoint_id: &str)`. Limits from Global Constraints.

- [ ] **Step 1: Write failing tests:** `file_store_roundtrip_and_delete` (tempdir), `file_store_is_owner_only` (`#[cfg(unix)]`, mode `0o600`), `trusted_devices_persist_across_load`, `retain_only_drops_removed`, `limiter_locks_after_five_failures` (5th failure → `check` returns `Err(d)` with `d <= 15 min`; after advancing `now` by 15 min → `Ok`), `limiter_global_lock_after_twenty_failures` (20 failures spread over different IDs → a fresh ID is locked), `limiter_success_resets_endpoint_counter`.
- [ ] **Step 2:** `cargo test -p remote_tunnel local_state` → FAIL.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** `cargo test -p remote_tunnel local_state` → PASS.
- [ ] **Step 5:** Commit `feat(remote_tunnel): add secret store, trusted devices and attempt limiter`.

---

### Task 4: GitHub device flow and gist registry

**Files:**
- Create: `crates/remote_tunnel/src/github.rs`
- Modify: `crates/remote_tunnel/Cargo.toml` (dev-dependency `http_client = { workspace = true, features = ["test-support"] }`)

**Interfaces:**
- Produces:
  - `pub const GITHUB_CLIENT_ID: &str` (value from Task 0), `pub fn client_id() -> String` (env `WU_GITHUB_CLIENT_ID` wins).
  - `pub struct DeviceCode { pub device_code: String, pub user_code: String, pub verification_uri: String, pub expires_in: u64, pub interval: u64 }`
  - `pub async fn request_device_code(http: &Arc<dyn HttpClient>) -> Result<DeviceCode>` (POST `https://github.com/login/device/code`, form `client_id`, `scope=gist`, `Accept: application/json`).
  - `pub async fn poll_for_token(http: &Arc<dyn HttpClient>, code: &DeviceCode, sleep: impl Fn(Duration) -> BoxFuture<'static, ()>) -> Result<String>` (POST `https://github.com/login/oauth/access_token`, `grant_type=urn:ietf:params:oauth:grant-type:device_code`; `authorization_pending` → wait `interval`; `slow_down` → interval += 5 s; `expired_token`, `access_denied` → error with user-facing text). `sleep` is injected so tests don't wait.
  - `pub struct GithubUser { pub id: u64, pub login: String }`, `pub async fn current_user(http, token: &str) -> Result<GithubUser>`.
  - `#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)] pub struct DeviceRecord { pub version: u32, pub endpoint_id: String, pub name: String, pub platform: PlatformInfo, pub wu_version: String, pub relay_url: Option<String>, pub direct_addresses: Vec<String>, pub accepts_connections: bool, pub updated_at: String /* RFC 3339 */ }`
  - `pub struct GistRegistry { http: Arc<dyn HttpClient>, token: String, gist_id: String }`:
    - `pub async fn open_or_create(http, token) -> Result<Self>` (GET `/gists?per_page=100` following `Link: rel="next"`, match description `wu-remote-tunnel devices`; else POST `/gists` with `public: false` and a `README.md` explaining the gist).
    - `pub async fn devices(&self) -> Result<Vec<DeviceRecord>>` (GET `/gists/{id}`, parse every `device-*.json`; skip and `log::warn!` files that fail to parse; if a file is `truncated`, fetch `raw_url`).
    - `pub async fn put_device(&self, record: &DeviceRecord) -> Result<()>` (PATCH only `device-<endpoint_id>.json`).
    - `pub async fn remove_device(&self, endpoint_id: &str) -> Result<()>` (PATCH with that file set to `null`).
  - `pub enum GithubError { Unauthorized, RateLimited { reset_at: Option<u64> }, Other(anyhow::Error) }` returned (via `anyhow` downcast) when status is 401 / 403-with-`x-ratelimit-remaining: 0`.

- [ ] **Step 1: Write failing tests** with `FakeHttpClient::create` asserting method, URL, headers and body, and returning canned JSON: `device_code_request_shape`, `poll_handles_pending_then_slow_down_then_token` (asserts the second wait is `interval + 5 s`), `poll_reports_access_denied`, `open_finds_existing_gist_on_second_page`, `open_creates_gist_when_missing` (body has `"public": false` and description), `put_device_patches_only_own_file`, `remove_device_sends_null_file`, `devices_skips_unparseable_file`, `unauthorized_maps_to_github_error`.
- [ ] **Step 2:** `cargo test -p remote_tunnel github` → FAIL.
- [ ] **Step 3:** Implement with `http_client::Request::builder()` + `HttpClient::send` (pattern: `crates/git_hosting_providers/src/providers/github.rs:136-160`).
- [ ] **Step 4:** `cargo test -p remote_tunnel github` → PASS.
- [ ] **Step 5:** Commit `feat(remote_tunnel): add GitHub device flow and gist registry`.

---

### Task 5: `remote_tunnel` settings

**Files:**
- Modify: `crates/settings_content/src/settings_content.rs` (field `pub remote_tunnel: Option<RemoteTunnelSettingsContent>` next to `call_hierarchy`; add `remote_tunnel` to the `fallible_options::flattened_deserialize!` options list; define `RemoteTunnelSettingsContent` following `CallHierarchySettingsContent` with doc comments ending in `Default: ...`)
- Modify: `crates/settings/src/vscode_import.rs` (`remote_tunnel: None`)
- Modify: `assets/settings/default.json` (`"remote_tunnel": { "relay_url": null, "allow_remote_access": false, "require_authenticator": true, "device_name": null }` with one-line `//` comments)
- Create: `crates/remote_tunnel/src/settings.rs`

**Interfaces:**
- Produces: `#[derive(Clone, Debug, PartialEq, RegisterSetting)] pub struct RemoteTunnelSettings { pub relay_url: Option<String>, pub allow_remote_access: bool, pub require_authenticator: bool, pub device_name: Option<String> }` with `impl Settings` using `content.remote_tunnel.clone().unwrap_or_default()` and per-field `unwrap_or(default)` (style of `crates/agent_chat/src/agent_chat_settings.rs`, no unwraps).

- [ ] **Step 1: Write failing test** `settings_defaults_and_override` (gpui test: load default settings → `require_authenticator == true`, `allow_remote_access == false`; apply user JSON `{"remote_tunnel": {"relay_url": "https://relay.example.com"}}` → `relay_url` set, other fields unchanged).
- [ ] **Step 2:** `cargo test -p remote_tunnel settings` → FAIL.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** `cargo test -p remote_tunnel settings` → PASS, and `cargo check -p settings` passes.
- [ ] **Step 5:** Commit `feat(remote_tunnel): add remote_tunnel settings`.

---

### Task 6: `TunnelService` (endpoint, sign-in state, device list, ping)

**Files:**
- Create: `crates/remote_tunnel/src/service.rs`
- Modify: `crates/remote_tunnel/Cargo.toml` (`gpui`, `gpui_tokio`, `iroh`, `tokio` with `["io-util", "net", "time", "sync"]`, `tokio-util` with `["compat"]`, `http_client`, `paths`, `release_channel`, `settings`, `serde`, `serde_json`, `futures`, `anyhow`, `log`, `util`)

**Interfaces:**
- Consumes: Tasks 1-5.
- Produces:
  - `pub struct TunnelConfig { pub profile: Profile, pub secret_store: SecretStore, pub relay_url: Option<String>, pub device_name: String, pub require_authenticator: bool, pub host_wu_version: String, pub proxy_spawner: Arc<dyn ProxySpawner> }`
  - `pub trait ProxySpawner: Send + Sync { fn spawn(&self, identifier: &str, reconnect: bool) -> Result<SpawnedProxy>; }` with `pub struct SpawnedProxy { pub stdin: Box<dyn AsyncWrite + Send + Unpin>, pub stdout: Box<dyn AsyncRead + Send + Unpin>, pub stderr: Box<dyn AsyncRead + Send + Unpin>, pub wait: BoxFuture<'static, Result<i32>> }`. The production impl `BinaryProxySpawner { binary: PathBuf }` runs `<binary> proxy --identifier <id> [--reconnect]` via `util::command::new_command(..).kill_on_drop(true)`.
  - `pub enum AuthState { SignedOut, SigningIn { user_code: String, verification_uri: String }, SignedIn { user: GithubUser } }`
  - `pub struct TunnelService` (gpui entity, registered as a global `GlobalTunnelService(Entity<TunnelService>)`), methods:
    - `pub fn init(config: TunnelConfig, http: Arc<dyn HttpClient>, cx: &mut App)` (loads or generates the iroh `SecretKey` and stores it under `SecretName::EndpointKey`; loads token; binds the endpoint on the gpui_tokio runtime with address lookup disabled and relay mode from `relay_url`).
    - `pub fn global(cx: &App) -> Option<Entity<Self>>`
    - `pub fn auth_state(&self) -> &AuthState`, `pub fn endpoint_id(&self) -> String`
    - `pub fn sign_in(&mut self, cx: &mut Context<Self>) -> Task<Result<()>>` (device flow; state goes `SigningIn` → `SignedIn`; writes own `DeviceRecord` with `accepts_connections` = current hosting state).
    - `pub fn sign_out(&mut self, cx: &mut Context<Self>) -> Task<Result<()>>` (removes own gist file, deletes token, stops hosting).
    - `pub fn devices(&self) -> &[DeviceRecord]` (excluding self), `pub fn refresh_devices(&mut self, cx) -> Task<Result<()>>`
    - `pub fn ping(&self, endpoint_id: &str, cx: &App) -> Task<bool>` (dial with `Purpose::Ping`, 3 s timeout; any `HostReply`, including `NeedsCode`, counts as reachable; never prompts for a code).
    - `pub fn remove_device(&mut self, endpoint_id: &str, cx) -> Task<Result<()>>`
    - `pub fn set_config(&mut self, config_update: ..., cx)` for relay URL / name / authenticator changes (rebinding the endpoint when `relay_url` changes).
    - `pub fn open_stream(&self, endpoint_id: &str, hello: ClientHello, cx: &App) -> Task<Result<(HostReply, TunnelStreams)>>` where `pub struct TunnelStreams { pub send: Box<dyn AsyncWrite + Send + Unpin>, pub recv: Box<dyn AsyncRead + Send + Unpin>, pub connection: iroh::endpoint::Connection }` (QUIC streams wrapped with `tokio_util::compat`).
  - Address publishing: subscribe to the endpoint's address watcher; on relay URL or direct-address change, PATCH own device file (debounce 30 s).
  - `pub fn test_pair(cx_a, cx_b) -> (Entity<TunnelService>, Entity<TunnelService>)` under `#[cfg(any(test, feature = "test-support"))]`: two services with `RelayMode::Disabled`, loopback direct addresses, a fake registry listing both devices, and an in-memory `ProxySpawner` (tokio `duplex`).

- [ ] **Step 1: Write failing tests:** `endpoint_key_is_stable_across_restarts` (init twice with the same `File` store → same `endpoint_id`), `ping_reports_reachable_peer` (test pair → `true`), `ping_reports_unreachable_peer_within_timeout` (unknown id → `false` in < 4 s using the gpui executor timer), `sign_out_removes_own_device_file`.
- [ ] **Step 2:** `cargo test -p remote_tunnel service` → FAIL.
- [ ] **Step 3:** Implement. The fake registry is a `trait DeviceRegistry` (methods `devices`, `put_device`, `remove_device`) implemented by `GistRegistry` and by a test fake; `TunnelService` stores `Arc<dyn DeviceRegistry>`.
- [ ] **Step 4:** `cargo test -p remote_tunnel service` → PASS.
- [ ] **Step 5:** Commit `feat(remote_tunnel): add TunnelService with iroh endpoint and device list`.

---

### Task 7: Hosting (accept loop, access check, proxy bridging)

**Files:**
- Create: `crates/remote_tunnel/src/host.rs`

**Interfaces:**
- Consumes: Task 6 (`TunnelService`, `ProxySpawner`, `DeviceRegistry`), Task 3 (`TrustedDevices`, `AttemptLimiter`), Task 1 (`verify_totp`).
- Produces:
  - `TunnelService::set_hosting(&mut self, enabled: bool, cx) -> Task<Result<()>>` (starts/stops the accept loop; updates `accepts_connections` in the gist).
  - `pub fn authenticator_setup(&self, cx: &App) -> Task<Result<AuthenticatorSetup>>` with `pub struct AuthenticatorSetup { pub otpauth_uri: String, pub qr_text: String }` (creates the TOTP secret on first call).
  - `pub fn reset_authenticator(&mut self, cx) -> Task<Result<()>>` (new secret, clears `TrustedDevices`).
  - `fn check_access(&mut self, peer: &str, code: Option<&str>, now: SystemTime) -> HostReply` decision order: snapshot rule (Review Focus 2) → not in gist ⇒ `Rejected` → authenticator off or peer trusted ⇒ continue → limiter locked ⇒ `LockedOut` → no code ⇒ `NeedsCode` → wrong ⇒ `WrongCode` → right ⇒ trust and continue → version differs ⇒ `VersionMismatch` → `Ok(HostInfo)`.
  - Proxy purpose: `proxy_spawner.spawn(identifier, reconnect)`, copy QUIC recv → child stdin and child stdout → QUIC send as raw bytes (the child already speaks the length-prefixed envelope protocol); child stderr goes to a QUIC uni stream; when either side closes, close the other.
  - Every 5 minutes while any connection is open: refetch devices, `retain_only`, and close connections whose peer is gone.

- [ ] **Step 1: Write failing tests** (test pair from Task 6, fake clock): `unknown_device_is_rejected`, `first_connection_needs_code`, `right_code_trusts_device_and_next_connection_skips_code`, `wrong_code_counts_down_then_locks_out`, `wrong_code_error_mentions_clock` (the client-facing message built from `WrongCode` contains "check the time"), `authenticator_disabled_skips_code`, `removed_device_connection_is_closed_on_recheck`, `access_check_uses_recent_snapshot_when_github_fails`, `access_check_rejects_when_snapshot_is_stale`, `access_check_rejects_after_unauthorized`, `proxy_bytes_roundtrip` (bytes written by the client arrive at the fake child's stdin and vice versa).
- [ ] **Step 2:** `cargo test -p remote_tunnel host` → FAIL.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** `cargo test -p remote_tunnel host` → PASS.
- [ ] **Step 5:** Commit `feat(remote_tunnel): add hosting with GitHub and authenticator access check`.

---

### Task 8: `Tunnel` transport in `crates/remote`

**Files:**
- Modify: `crates/remote/Cargo.toml` (`remote_tunnel`, `gpui_tokio`)
- Create: `crates/remote/src/transport/tunnel.rs`
- Modify: `crates/remote/src/transport.rs` (`pub mod tunnel;`)
- Modify: `crates/remote/src/remote.rs` (re-export `TunnelConnectionOptions`)
- Modify: `crates/remote/src/remote_client.rs` (variant `Tunnel(TunnelConnectionOptions)` at :1314; arms in `ConnectionPool::connect` :1260, `display_name` :1323, `connection_type` :1336 → `"tunnel"`, `host` :1346 → `name`; `From<TunnelConnectionOptions>` next to :1541)
- Modify: `crates/remote/src/remote_identity.rs` (`RemoteConnectionIdentity::Tunnel { endpoint_id }`, `persistence_key`, `From` arm)
- Modify (compile-required arms): `crates/project/src/trusted_worktrees.rs:161`, `crates/recent_projects/src/recent_projects.rs:1899`, `crates/recent_projects/src/remote_connections.rs:398`, `crates/recent_projects/src/remote_servers.rs:199`, `crates/remote_connection/src/remote_connection.rs:243` (→ `(name, None, false)`), `crates/title_bar/src/title_bar.rs:614`, `crates/workspace/src/workspace.rs:8973` (add a real `Tunnel` arm comparing `endpoint_id`, do not rely on `_ => false`)

**Interfaces:**
- Consumes: Task 6 `TunnelService::open_stream`, Task 2 types.
- Produces:
  - `#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)] pub struct TunnelConnectionOptions { pub endpoint_id: String, pub name: String }`
  - `pub(crate) struct TunnelRemoteConnection` with `pub(crate) async fn new(options: TunnelConnectionOptions, delegate: Arc<dyn RemoteClientDelegate>, cx: &mut AsyncApp) -> Result<Self>`: dials with `Purpose::Ping` to obtain `HostInfo`; on `NeedsCode` / `WrongCode` asks the user through `delegate.ask_password("Authenticator code for {name}", ..)` (decrypt with `IKnowWhatIAmDoingAndIHaveReadTheDocs`) and redials with `authenticator_code` (max 3 prompts); maps `LockedOut`, `VersionMismatch` (message: `"{name} runs wu {host}, this wu is {client}: update wu on {name}"`), `Rejected` to errors.
  - `RemoteConnection` impl: `start_proxy` opens `Purpose::Proxy { identifier, reconnect }` and pumps envelopes with `protocol::write_message` / `read_message` over the compat streams (same shape as `handle_rpc_messages_over_child_process_stdio` at `transport.rs:127`, including `connection_activity_tx` and stderr log lines prefixed `(remote)`), returning the remote exit code or `1` when the connection drops; `path_style` from `HostInfo.platform.os`; `remote_platform`, `remote_os_version`, `shell`, `default_system_shell` from `HostInfo`; `has_wsl_interop` false; `shares_network_interface` false; `build_forward_ports_command` and `build_command` with `port_forward: Some(..)` return `Err(anyhow!("port forwarding is not supported over remote tunnel yet"))`; `upload_directory` returns the same kind of error (callers: extension upload, which is not in scope); `build_command` is implemented in Task 10 (until then return the same "not supported yet" error); `kill` closes the connection.
  - Client expected version: the same value `ssh.rs` passes when choosing the server binary (extract to a shared `pub(crate) fn expected_server_version(cx) -> String` in `transport.rs` if needed); compare with `HostInfo.host_wu_version`.

- [ ] **Step 1: Write failing tests** in `tunnel.rs` using `TunnelService::test_pair` with a fake proxy that echoes envelopes: `proxy_envelope_roundtrip` (send a `proto::Ping` envelope, receive it back via `incoming_tx`), `version_mismatch_reports_both_versions`, `needs_code_prompts_delegate_and_retries` (a test delegate supplies the right code on the first prompt), `tunnel_reconnect_reuses_daemon` (drop the connection, call `start_proxy` with `reconnect = true`, assert the fake spawner saw `reconnect = true` and the fake daemon instance count is still 1).
- [ ] **Step 2:** `cargo test -p remote tunnel` → FAIL.
- [ ] **Step 3:** Implement the transport and all match arms listed above.
- [ ] **Step 4:** `cargo test -p remote` → PASS, `cargo check --workspace` → no errors, `cargo test -p remote_server` → existing tests still PASS.
- [ ] **Step 5:** Commit `feat(remote): add tunnel transport`.

---

### Task 9: Persist tunnel connections

**Files:**
- Modify: `crates/workspace/src/persistence/model.rs` (`RemoteConnectionKind::Tunnel` ↔ `"tunnel"`)
- Modify: `crates/workspace/src/persistence.rs` (`get_or_create_remote_connection_internal` :1700: kind `Tunnel`, `host = endpoint_id`, name in the existing unused `name` column; tunnel rows are looked up by `kind + host` only and their `name` updated; `remote_connection_from_row` :1926 reads `host` and `name` back into `TunnelConnectionOptions`; extend the SELECT in `remote_connections` / `remote_connection` to include `name`)

**Interfaces:**
- Consumes: Task 8 `TunnelConnectionOptions`, `RemoteConnectionIdentity::Tunnel`.

- [ ] **Step 1: Write failing tests** next to the existing SSH persistence tests: `tunnel_connection_roundtrip` (save options `{ endpoint_id: "abc", name: "Server" }` → read back equal), `tunnel_rename_keeps_same_id` (same `endpoint_id` with name `"Server 2"` returns the same `RemoteConnectionId` and reads back `"Server 2"`).
- [ ] **Step 2:** `cargo test -p workspace tunnel_` → FAIL.
- [ ] **Step 3:** Implement (no schema migration needed: `name` already exists).
- [ ] **Step 4:** `cargo test -p workspace` → PASS.
- [ ] **Step 5:** Commit `feat(workspace): persist tunnel remote connections`.

---

### Task 10: Terminals and tasks over the tunnel (`tunnel-exec`)

**Files:**
- Create: `crates/remote_tunnel/src/exec.rs` (host PTY side and the GUI local-socket relay)
- Modify: `crates/remote_tunnel/Cargo.toml` (`portable-pty`, `net`, `terminal_size = "0.4"`)
- Modify: `Cargo.toml` (`terminal_size = "0.4"` workspace dependency)
- Modify: `crates/remote/src/transport/tunnel.rs` (`build_command`)
- Modify: `crates/remote_server/src/server.rs` (`Commands::TunnelExec { #[arg(long)] endpoint: String, #[arg(long)] cwd: Option<String>, #[arg(long = "env")] env: Vec<String> /* KEY=VALUE */, #[arg(long)] interactive: bool, #[arg(last = true)] command: Vec<String> }` and `fn execute_tunnel_exec(..) -> Result<i32>`)

**Interfaces:**
- Consumes: Task 6 `open_stream`, Task 2 `ExecRequest`, `ExecControl`.
- Produces:
  - Host: `Purpose::Exec(request)` → interactive: `portable_pty::native_pty_system().openpty(PtySize { rows, cols, .. })`, spawn `program` (or the host's default shell when `None`) with `args`, `env`, `cwd`; QUIC recv → PTY writer, PTY reader → QUIC send; `ExecControl::Resize` on a client uni stream → `master.resize`; on exit send `ExecControl::Exit { code }` on a host uni stream. Non-interactive: plain pipes via `util::command::new_command`, stderr merged into the same send stream.
  - GUI relay: `pub fn start_exec_relay(cx: &mut App) -> Result<ExecRelayInfo>` listening on `net::async_net::UnixListener` at `state_dir(App)/exec.sock`; `pub struct ExecRelayInfo { pub socket_path: PathBuf, pub token: String /* 32 random bytes hex */ }`. A client sends `{ token, endpoint_id, request: ExecRequest }` as length-prefixed JSON; wrong token closes the socket. The relay calls `open_stream(endpoint_id, Purpose::Exec)` and splices bytes and control frames.
  - `TunnelRemoteConnection::build_command` returns `CommandTemplate { program: <local remote_server binary>, args: ["tunnel-exec", "--endpoint", id, ("--cwd", dir)?, ("--env", "K=V")*, ("--interactive")?, "--", program?, args..], env: { "WU_TUNNEL_EXEC_SOCKET": path, "WU_TUNNEL_EXEC_TOKEN": token } }`; `port_forward: Some(..)` → the error from Task 8. The local binary is obtained with the delegate's `download_server_binary_locally` for the local platform and cached for the session.
  - `tunnel-exec` (client side): connects to the socket, sends the request with the current `terminal_size` (default 80x24), puts the local terminal in raw mode when `--interactive` (`nix::sys::termios` on Unix; `SetConsoleMode` without line input/echo/processed input and with virtual terminal input on Windows; restore on exit, including on error), copies stdin/stdout, polls `terminal_size` every 200 ms and sends `Resize` on change, exits with the host's exit code.

- [ ] **Step 1: Write failing tests** (host side via the test pair and the in-process relay, no real TTY): `exec_non_interactive_returns_output_and_exit_code` (run `["sh", "-c", "echo hi; exit 3"]` on Unix / `["cmd", "/C", "echo hi & exit 3"]` on Windows → output contains `hi`, exit code `3`), `exec_interactive_applies_resize` (start `stty size` on Unix after a `Resize { columns: 100, rows: 40 }` → output `40 100`; `#[cfg(unix)]`), `relay_rejects_wrong_token`, `build_command_shape` (args and env exactly as above for a sample request), `build_command_rejects_port_forward`.
- [ ] **Step 2:** `cargo test -p remote_tunnel exec` and `cargo test -p remote build_command` → FAIL.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** Both commands → PASS.
- [ ] **Step 5:** Commit `feat(remote_tunnel): run terminals and tasks over the tunnel`.

---

### Task 11: `remote_server` CLI (`login`, `logout`, `tunnel`, `remove`)

**Files:**
- Modify: `crates/remote_server/Cargo.toml` (`remote_tunnel`)
- Modify: `crates/remote_server/src/server.rs` (new `Commands` variants and handlers)
- Modify: `crates/remote_server/src/main.rs` (usage string → `usage: remote <run|proxy|version|login|logout|tunnel|remove|tunnel-exec>`)
- Create: `docs/src/remote-tunnel.md` (user docs: GUI flow, headless flow, sample systemd unit, custom relay, removing a machine) and link it from the docs index next to the remote development page

**Interfaces:**
- Consumes: Tasks 6, 7.
- Produces:
  - `Commands::Login`, `Commands::Logout`, `Commands::Tunnel { #[arg(long)] name: Option<String>, #[arg(long)] relay_url: Option<String>, #[arg(long)] no_authenticator: bool, #[arg(long)] reset_authenticator: bool }`, `Commands::Remove { name_or_id: String }`.
  - `fn with_headless_tunnel<R>(config: TunnelConfig, run: impl FnOnce(Entity<TunnelService>, &mut AsyncApp) -> Task<Result<R>>) -> Result<R>`: builds `gpui_platform::headless()`, `gpui_tokio::init`, a `ReqwestClient` inside `Tokio::handle(cx).enter()` (pattern `server.rs:578-593`), `TunnelService::init` with `Profile::Server`, `SecretStore::File(state_dir(Server))`, `host_wu_version = VERSION.clone()`, `BinaryProxySpawner { binary: std::env::current_exe()? }`.
  - `login`: prints `Open {verification_uri} and enter the code {user_code}` and waits; prints `Signed in as {login}`.
  - `tunnel`: refuses to start when signed out (`Run 'remote_server login' first`); with the authenticator on and no secret yet (or `--reset-authenticator`), prints the QR text, the `otpauth://` URI and `Scan this with your authenticator app`; then hosts until Ctrl+C, setting `accepts_connections: false` on graceful exit.
  - `remove`: matches by exact name or endpoint ID prefix (error when ambiguous) and deletes that device file.

- [ ] **Step 1: Write failing tests:** `cli_parses_new_subcommands` (clap `try_parse_from` for each variant with its flags), `remove_resolves_unique_prefix_and_rejects_ambiguous` (pure function `resolve_device(&[DeviceRecord], &str) -> Result<&DeviceRecord>`).
- [ ] **Step 2:** `cargo test -p remote_server cli_` → FAIL.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** `cargo test -p remote_server` → PASS; manual: `cargo run -p remote_server -- login` shows a real device code.
- [ ] **Step 5:** Commit `feat(remote_server): add login, tunnel and remove commands`.

---

### Task 12: GUI ("Your machines", sign-in, remote access, QR)

**Files:**
- Modify: `crates/recent_projects/Cargo.toml` (`remote_tunnel`)
- Modify: `crates/recent_projects/src/remote_servers.rs` (new `RemoteMatch` variants `MachinesHeader`, `SignIn`, `Machine { device: DeviceRecord, reachable: Option<bool> }`, `AllowRemoteAccess`; pushed first in `rebuild_matches` :665; rendered in `render_match` :1008; handled in `confirm` :904; new `Mode::GithubSignIn(GithubSignInState)` and `Mode::AuthenticatorSetup(AuthenticatorSetup)` following `Mode::CreateRemoteServer`; selecting a `Machine` calls `create_remote_project` with `RemoteConnectionOptions::Tunnel(..)`; a per-row "Remove this machine" action)
- Modify: `crates/recent_projects/src/recent_projects.rs` (`actions!` additions: `SignInWithGithub`, `SignOutOfGithub`, `ToggleRemoteAccess`, `ShowAuthenticatorQrCode` in a `remote_tunnel` namespace, registered in `init`)
- Modify: `crates/wu/src/main.rs` (after `gpui_tokio::init` at :475: `remote_tunnel::TunnelService::init` with `Profile::App`, `SecretStore::Keychain`, `cx.http_client()`, settings-derived config, `BinaryProxySpawner` using a `remote_server` binary for the local platform, obtained with the same function the `RemoteClientDelegate` impl in `crates/remote_connection/src/remote_connection.rs:440` uses for `download_server_binary_locally` (dev builds: the `build_remote_server_from_source` path in `crates/remote/src/transport.rs:241`), fetched lazily the first time hosting is enabled; start the exec relay; observe `SettingsStore` and call `set_config` / `set_hosting` when `remote_tunnel` settings change)

**Interfaces:**
- Consumes: Tasks 5, 6, 7, 8, 10.
- UI copy (exact): header `Your machines`; signed out row `Sign in with GitHub`; sign-in dialog `Enter this code on GitHub:` + the code + `Copy` button (`cx.write_to_clipboard(ClipboardItem::new_string(..))`, toast `Code copied`) + `Open GitHub` button (`cx.open_url(verification_uri)`); toggle `Allow remote access to this machine`; QR dialog title `Scan with your authenticator app`, the QR text in the buffer font, the `otpauth://` URI with `Copy`, buttons `Done` and `Skip (no authenticator)` (Skip writes `require_authenticator: false` via `update_settings_file`); row action `Remove this machine`; unreachable machines are dimmed with tooltip `Offline or not accepting connections`.
- `fn machine_label(device: &DeviceRecord, all: &[DeviceRecord]) -> String`: `name`, plus ` (<platform>, <first 8 chars of endpoint_id>)` when another device has the same name.

- [ ] **Step 1: Write failing tests:** `duplicate_names_show_short_id` (pure `machine_label`), `signed_out_shows_sign_in_row` and `signed_in_lists_other_machines_first` (gpui test building `RemoteServerPickerDelegate` with a test `TunnelService` and asserting the `RemoteMatch` order: `MachinesHeader`, `AllowRemoteAccess`, `Machine` rows, then the existing SSH entries).
- [ ] **Step 2:** `cargo test -p recent_projects machine` → FAIL.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** `cargo test -p recent_projects` → PASS. Manual on two real machines on different networks (one behind NAT), following the spec's Testing section: sign in on both, enable remote access and scan the QR on the host, connect from the other (code asked once, not on the second connect), edit a file, see LSP diagnostics, open a terminal and resize it, run a task, disconnect the network for 30 s and confirm reconnect, remove the client machine from the host and confirm the open session closes within 5 minutes.
- [ ] **Step 5:** Commit `feat(recent_projects): add Your machines section for remote tunnel`.

---

### Task 13: Final verification

- [ ] **Step 1:** `./script/clippy` → no warnings in touched crates.
- [ ] **Step 2:** `cargo test -p remote_tunnel -p remote -p workspace -p remote_server -p recent_projects` → all PASS.
- [ ] **Step 3:** Confirm SSH remote still works manually (connect to an existing SSH host, open a terminal).
- [ ] **Step 4:** Commit any clippy fixes `chore(remote_tunnel): clippy fixes`.
