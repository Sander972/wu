# Dependency Upgrade Merge Plan

**Goal:** Merge branch `dep-upgrade` (33 commits of dependency bumps, worktree `E:\Ale\wu-dep-upgrade`) into `main` (106 commits since the merge base `faf1774`, mostly the Claude parity work), so that `main` builds, lints and tests as cleanly as before the merge.

**Background:** The upgrade's own notes are in `docs/dep-upgrade_hand-off.md` (arrives with the merge). It lists every bump, the code changes each one needed, the known risks and the manual test checklist.

## Progress (2026-11-27)

- Plan written. Nothing merged yet.

## Analysis

- `git merge-tree` dry run: only `Cargo.toml` and `Cargo.lock` conflict. `crates/agent_harness/src/model_context.rs` and `crates/terminal/src/terminal.rs` changed on both sides but merge cleanly.
- `main` already has part of the upgrade: `a00b1cc` committed 18 of the bumps (session 1 of the upgrade). Every line where `main`'s `Cargo.toml` differs from `dep-upgrade`'s is an older version that `dep-upgrade` bumped, so `dep-upgrade`'s `Cargo.toml` is a strict superset.
- Semantic conflicts (merge cleanly, fail to compile): code added on `main` after the base that uses APIs the bumps changed.
  - `crates/agent_harness/src/model_context.rs`, `ModelContext::with_launch`: `format!("{:x}", hash.finalize())` no longer compiles with sha2 0.11 (`dep-upgrade` uses `hex::encode` for the same pattern).
  - `crates/agent_chat` (IDE server), `process_is_alive`: `sysinfo::System::new_with_specifics` returns `Result` in sysinfo 0.40.
  - Unknown others (futures-lite 2, itertools 0.15, toml 1 and so on): found by the full workspace check.
- Known clippy failures on `main` before the merge (not ours): `clippy::redundant_clone` at `crates/agent_harness/src/usage.rs:171` and `clippy::borrowed_box` at `crates/workspace/src/status_bar.rs:220`.
- Known flaky or hanging tests before the merge (not ours): `auto_update` (hangs), markdown `test_metadata_blocks_store_table_rows`, extension_host `test_extension_store_with_test_extension` (timeout), agent_chat `send_now_shows_the_message_and_queue_order_survives_idle` (fails under full-suite load).

## Constraints

- Merge in the main checkout on `main` with `git merge --no-ff`. No rebase, no amend, never `git push`.
- Build with `CARGO_TARGET_DIR=E:\Ale\wu-dep-upgrade\target`: its third-party dependencies are already compiled at the new versions, and E: has only ~73 GB free.
- Any new failure is first compared against `main` before the merge (the commit before the merge), and only fixed if the merge caused it.
- Subagents, if any, run on Sonnet.

## Tasks

### Task 1: Prepare

- [ ] **Step 1:** Confirm `E:\Ale\wu-dep-upgrade` has no uncommitted changes and `dep-upgrade` is at `97359c6`.
- [ ] **Step 2:** Clear the two metadata-only "modified" files on `main` (`git update-index --refresh`); keep `claude-parity_session-report.md` untracked and untouched.

### Task 2: Merge and resolve the manifest conflicts

- [ ] **Step 1:** `git merge --no-ff --no-commit dep-upgrade`.
- [ ] **Step 2:** Take `dep-upgrade`'s `Cargo.toml` (`git checkout --theirs Cargo.toml`).
- [ ] **Step 3:** Take `dep-upgrade`'s `Cargo.lock`, then run `cargo metadata` (or the first `cargo check`) so cargo adds the dependency edges `main`'s `agent_chat` gained (`async-tungstenite`, `lsp`, `smol`, `sysinfo`, `terminal`, `wu_actions`).
- [ ] **Step 4:** Verify the `Cargo.lock` diff against `dep-upgrade` only adds those edges: no package versions change.

### Task 3: Fix the semantic conflicts

- [ ] **Step 1:** `cargo check -p agent_harness -p agent_chat -p terminal --all-targets`.
- [ ] **Step 2:** `model_context.rs`: `hex::encode(hash.finalize())` in `with_launch`.
- [ ] **Step 3:** `process_is_alive`: handle the `Result` from `System::new_with_specifics` (an error means "can't tell", so pick the answer that is safe for its caller).
- [ ] **Step 4:** Fix whatever else Step 1 reports, then rerun it until clean.

### Task 4: Full verification

- [ ] **Step 1:** `cargo check --workspace --all-targets` is clean.
- [ ] **Step 2:** `./script/clippy` shows only the two known lints listed above.
- [ ] **Step 3:** Tests for the crates both branches touched: `agent_harness`, `agent_chat`, `terminal`, `sqlez`, `util`, `rpc`, `language`, `editor`, `markdown`, `dictation`, `git_ui`. Every failure is checked against the known flaky list and against pre-merge `main`.
- [ ] **Step 4:** `rustfmt --edition 2024` on files touched by the fixes.

### Task 5: Commit

- [ ] **Step 1:** One merge commit containing the conflict resolution and the Task 3 fixes, so every commit on `main` builds. The message lists the fixes.
- [ ] **Step 2:** Update this plan's Progress section and commit it.

### Task 6: Manual smoke test (user)

- [ ] **Step 1:** `cargo run -p wu -- --user-data-dir <copy of real data>` (see `docs/dep-upgrade_hand-off.md`, "Manual testing").
- [ ] **Step 2:** Check: the Claude chat (IDE server, model picker, accounts), the terminal (extra env), restored workspaces and databases (SQLite bump), SVG icons and Mermaid diagrams (resvg 0.50), editor hover, dictation and microphone selection, extensions.

### Task 7: Clean up (only with the user's OK)

- [ ] **Step 1:** `git worktree remove ../wu-dep-upgrade` and delete its `target/` (~215 GB).
- [ ] **Step 2:** `cargo trim` on `E:\Ale\wu\target` (~105 GB, mostly stale old-dependency builds).
- [ ] **Step 3:** Decide whether to delete the local `dep-upgrade` branch (`origin/dep-upgrade` is behind it, at `528d7cb`).

## Not covered

- macOS and Linux: the upgrade was only cross-checked with zig; a real CI run is still needed.
- The open items from the upgrade hand-off (etw_tracing on windows 0.61, extension_cli's wasmtime 48, oo7's duplicate ashpd/zbus, the two Intel Vulkan wgpu test crashes) stay open.
