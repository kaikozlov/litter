# Validation Contract: Agent Detection Wiring in SSH Connect Flow

**Milestone:** detection-wiring  
**Date:** 2026-04-13  
**Status:** draft

---

## Scope

Wire agent auto-detection (`detect_pi_agent` + `detect_droid_agent`) into the SSH connect flow so that:
1. A combined `detect_all_agents(ssh_client)` helper runs Pi + Droid detection in parallel over an established SSH session.
2. The guided SSH connect flow calls detection after SSH auth succeeds, before Codex bootstrap.
3. The provider SSH connect path validates the selected agent exists before spawning transport.
4. iOS shows an agent picker when multiple agents are detected after SSH connect.
5. Detection falls through to Codex-only flow when only Codex (or nothing else) is found.

### Existing Code (unchanged)

| Component | Location | Role |
|-----------|----------|------|
| `detect_pi_agent()` | `shared/rust-bridge/codex-mobile-client/src/provider/pi/detection.rs` | Probes SSH host for Pi binary, version, ACP availability |
| `detect_droid_agent()` | `shared/rust-bridge/codex-mobile-client/src/provider/droid/detection.rs` | Probes SSH host for Droid binary, version, common install paths |
| `AgentType` enum | `shared/rust-bridge/codex-mobile-client/src/provider/mod.rs` | Codex, PiAcp, PiNative, DroidAcp, DroidNative, GenericAcp |
| `AgentInfo` record | `shared/rust-bridge/codex-mobile-client/src/provider/mod.rs` | id, display_name, description, detected_transports, capabilities |
| `SshClient::exec()` | `shared/rust-bridge/codex-mobile-client/src/ssh.rs` | Execute remote command, returns `ExecResult` |
| `run_guided_ssh_connect()` | `shared/rust-bridge/codex-mobile-client/src/ffi/ssh.rs` | Guided SSH flow: connect → find codex → install? → bootstrap |
| `run_provider_ssh_connect()` | `shared/rust-bridge/codex-mobile-client/src/ffi/ssh.rs` | Provider path for non-Codex agents via `connect_remote_over_ssh_with_agent_type` |
| `create_provider_over_ssh()` | `shared/rust-bridge/codex-mobile-client/src/provider/factory.rs` | Factory: AgentType → transport over SSH exec_stream |
| `is_non_codex_agent()` | `shared/rust-bridge/codex-mobile-client/src/ffi/ssh.rs` | Routes None/Codex to guided path, others to provider path |

### New Code (to create)

| Component | Location | Purpose |
|-----------|----------|---------|
| `detect_all_agents(ssh_client)` | New: `shared/rust-bridge/codex-mobile-client/src/provider/detection.rs` | Parallel Pi + Droid detection, returns `Vec<AgentInfo>` with Codex always included |
| Detection call in guided flow | Modify: `ffi/ssh.rs` → `run_guided_ssh_connect()` | After SSH auth, before Codex bootstrap, call detection and surface results |
| Detection call in provider flow | Modify: `ffi/ssh.rs` → `run_provider_ssh_connect()` | Validate selected agent binary exists before spawning transport |
| iOS agent picker | New: `apps/ios/Sources/Litter/Views/AgentPickerSheet.swift` | SwiftUI sheet showing detected agents when multiple found |
| Store update for detected agents | Modify: Rust store | New state field for detected agents per server, surfaced to iOS via UniFFI |

---

## Assertions

### Area: Detection Helper

#### VAL-DETECT-001: detect_all_agents returns Codex when no Pi/Droid found

**Behavioral description:** When the remote host has neither `pi` nor `droid` binaries on PATH (and no common install locations), `detect_all_agents(ssh_client)` returns a single-element list containing `AgentInfo { id: "codex", display_name: "Codex", detected_transports: [AgentType::Codex], ... }`.

**Tool:** `cargo test` — unit test with mocked SSH responses (pi/droid probes return exit_code != 0).  
**Evidence:** Test asserts `result.len() == 1` and `result[0].detected_transports == vec![AgentType::Codex]`.

---

#### VAL-DETECT-002: detect_all_agents returns Pi types when pi binary found

**Behavioral description:** When `command -v pi` returns a path and `pi --version` returns a valid version string, the result includes `AgentInfo` entries for `AgentType::PiNative`. If `npx pi-acp --version` also succeeds, `AgentType::PiAcp` is also included in the same agent's `detected_transports`.

**Tool:** `cargo test` — unit test with mocked SSH exec that returns pi path + version.  
**Evidence:** Test asserts result contains an `AgentInfo` with `PiNative` (and optionally `PiAcp`) in `detected_transports`.

---

#### VAL-DETECT-003: detect_all_agents returns Droid types when droid binary found

**Behavioral description:** When `command -v droid` returns a path and `droid --version` returns a valid version string, the result includes an `AgentInfo` entry with `AgentType::DroidNative` in `detected_transports`.

**Tool:** `cargo test` — unit test with mocked SSH exec that returns droid path + version.  
**Evidence:** Test asserts result contains an `AgentInfo` with `DroidNative` in `detected_transports`.

---

#### VAL-DETECT-004: detect_all_agents returns all types when both Pi and Droid found

**Behavioral description:** When both Pi and Droid binaries are present and functional on the remote host, `detect_all_agents` returns entries for both agents plus the always-present Codex entry.

**Tool:** `cargo test` — unit test with mocked SSH responses for both pi and droid probes succeeding.  
**Evidence:** Test asserts `result.len() >= 3` (Codex + Pi + Droid), and each entry has non-empty `detected_transports`.

---

#### VAL-DETECT-005: detect_all_agents runs Pi and Droid detection in parallel

**Behavioral description:** The Pi and Droid detection futures are `tokio::join!`ed (or equivalent), not awaited sequentially. This is verified by code review — the implementation must use `tokio::join!` or `futures::join!` to run both probes concurrently over the same SSH session.

**Tool:** Code review of `provider/detection.rs`.  
**Evidence:** The `detect_all_agents` function body uses `tokio::join!` with `detect_pi_agent` and `detect_droid_agent`. Sequential `await` chains are not acceptable.

---

#### VAL-DETECT-006: detect_all_agents is fast — completes in <5 seconds

**Behavioral description:** Total detection time is bounded by `tokio::time::timeout(Duration::from_secs(5), detect_all_agents(ssh_client))`. If either probe hangs, the timeout fires and partial results are returned (with Codex as fallback).

**Tool:** `cargo test` — unit test verifying timeout wrapping; manual verification against gvps host.  
**Evidence:** Test asserts that a 5-second `tokio::time::timeout` wraps the detection call. Timeout returns partial results including at minimum the Codex entry.

---

### Area: Detection in Guided SSH Connect

#### VAL-GUIDED-001: detection runs after SSH auth, before Codex bootstrap

**Behavioral description:** In `run_guided_ssh_connect()`, the detection call is inserted after `SshClient::connect()` succeeds and before `resolve_codex_binary_optional_with_shell()` is called. The connection progress UI shows a new step `AppConnectionStepKind::DetectingAgents` between `ConnectingToSsh` (completed) and `FindingCodex` (pending).

**Tool:** Code review of `ffi/ssh.rs` → `run_guided_ssh_connect()`.  
**Evidence:** Detection call appears in the function body after SSH connect success log and before the `resolve_codex_binary_optional_with_shell` call. A new progress step `DetectingAgents` is emitted.

---

#### VAL-GUIDED-002: when only Codex found, proceeds with normal Codex bootstrap (unchanged)

**Behavioral description:** When `detect_all_agents` returns only the Codex entry (no Pi, no Droid), the guided flow continues exactly as before: resolve codex binary → install if missing → bootstrap app server → forward port → finish connect. No agent picker is shown on iOS.

**Tool:** `cargo test` — integration test; manual verification against Codex-only host.  
**Evidence:** Test asserts that when `detected_agents.len() == 1 && detected_agents[0] matches Codex`, the function proceeds to the existing `resolve_codex_binary_optional_with_shell` call without branching to a picker flow.

---

#### VAL-GUIDED-003: when multiple agents found, returns detected agent list for iOS picker

**Behavioral description:** When `detect_all_agents` returns 2+ entries (e.g., Codex + Pi, or Codex + Pi + Droid), the guided flow stores the detected agents in the Rust store and updates the connection progress to surface an `AppConnectionStepKind::AgentSelection` step with state `AwaitingUserInput`. The iOS layer reads this and presents an agent picker.

**Tool:** `cargo test` — test that store receives detected agents; code review of store update.  
**Evidence:** Test asserts `app_store.update_detected_agents(server_id, agents)` is called with the full list when `agents.len() >= 2`.

---

#### VAL-GUIDED-004: detection failure (timeout/error) falls through to Codex path

**Behavioral description:** If `detect_all_agents` encounters an SSH error, probe timeout, or any unexpected failure, the function catches the error, logs a warning, and returns the Codex-only fallback list. The guided flow proceeds with normal Codex bootstrap. Detection errors never block the connection.

**Tool:** `cargo test` — unit test injecting SSH exec error during detection.  
**Evidence:** Test asserts that when SSH exec returns `SshError::Timeout`, `detect_all_agents` returns `[codex_agent_info]` and does not propagate the error.

---

#### VAL-GUIDED-005: existing Codex-only flow is unchanged when no detection issues

**Behavioral description:** All existing `cargo test` in `codex-mobile-client` continue to pass (0 regressions). The `is_non_codex_agent(None)` and `is_non_codex_agent(Some(AgentType::Codex))` paths remain unchanged. The guided flow's Codex-specific steps (FindingCodex, InstallingCodex, StartingAppServer, OpeningTunnel, Connected) are identical in sequence and behavior when no non-Codex agents are detected.

**Tool:** `cargo test --lib` — full test suite; `make ios-sim-fast` — iOS build.  
**Evidence:** All existing tests pass. Build succeeds. No new warnings in affected files.

---

### Area: Detection in Provider SSH Connect

#### VAL-PROVIDER-001: detection validates selected agent exists before spawning transport

**Behavioral description:** When `run_provider_ssh_connect()` receives an `agent_type` (e.g., `AgentType::PiNative`), it first SSH-connects, then calls a targeted detection check (e.g., `command -v pi`) to verify the agent binary actually exists on the host. Only after validation passes does it call `create_provider_over_ssh()`.

**Tool:** `cargo test` — test with mocked SSH returning binary found/not found.  
**Evidence:** Test asserts that when `ssh_client.exec("command -v pi")` returns exit_code 0, `create_provider_over_ssh` is called; when exit_code != 0, a clear error is returned before transport creation.

---

#### VAL-PROVIDER-002: if selected agent not found, returns clear error

**Behavioral description:** When the targeted detection check fails (binary not found on remote host), `run_provider_ssh_connect()` returns a `ClientError::Transport` with a descriptive message like `"Pi agent not found on remote host: pi binary not on PATH"`. The SSH session is disconnected cleanly.

**Tool:** `cargo test` — test with mocked SSH returning binary not found.  
**Evidence:** Test asserts the returned error is `ClientError::Transport` containing the agent name and "not found". The SSH session's `disconnect()` is called before returning.

---

#### VAL-PROVIDER-003: if selected agent found, proceeds with provider factory

**Behavioral description:** When validation confirms the agent binary exists, `run_provider_ssh_connect()` proceeds to call `mobile_client.connect_remote_over_ssh_with_agent_type()` with the validated agent_type, which internally uses `create_provider_over_ssh()` to spawn the transport.

**Tool:** `cargo test` — test with mocked SSH returning binary found; code review.  
**Evidence:** Test asserts the flow reaches `connect_remote_over_ssh_with_agent_type` after validation succeeds.

---

### Area: iOS Detection Flow

#### VAL-IOS-DETECT-001: iOS receives detected agent list when multiple found

**Behavioral description:** When the Rust store's `update_detected_agents` is called with 2+ agents for a server, the iOS `AppModel` observation layer receives the update and publishes the list to SwiftUI. The detected agents appear in the snapshot accessible from the UI thread.

**Tool:** Code review of `AppModel` → detected agents observation; `make ios-sim-fast` build.  
**Evidence:** The `AppModel` class exposes an `@Published` or equivalent property for detected agents. The UniFFI-generated `AgentInfo` Swift type is available.

---

#### VAL-IOS-DETECT-002: agent picker sheet shown with detected agents

**Behavioral description:** When `AppConnectionStepKind::AgentSelection` / `AwaitingUserInput` is active for a server and detected agents are available, an `AgentPickerSheet` is presented as a SwiftUI sheet. The sheet lists each detected agent with its display name, description, and transport types.

**Tool:** Code review of `AgentPickerSheet.swift`; manual verification in simulator.  
**Evidence:** The `AgentPickerSheet` view receives `[AgentInfo]` and renders a list. Each row shows `agent.display_name`, `agent.description`, and the available transport type(s).

---

#### VAL-IOS-DETECT-003: selecting an agent reconnects with that agent type

**Behavioral description:** When the user selects an agent from the picker, the selection triggers a new connection flow: the current SSH session is disconnected, and a new connection is initiated with the selected `AgentType`. For non-Codex agents, this goes through `run_provider_ssh_connect()`; for Codex, it goes through the guided flow.

**Tool:** Code review + manual verification in simulator.  
**Evidence:** The picker's on-select handler calls `sshBridge.sshStartRemoteServerConnect(...)` with the chosen `AgentType`. The previous connection attempt is cancelled/disconnected first.

---

#### VAL-IOS-DETECT-004: iOS builds with detection changes (make ios-sim-fast)

**Behavioral description:** The full iOS simulator build (`make ios-sim-fast`) completes successfully with no compile errors after all detection changes are integrated: new Rust detection module, updated FFI, new SwiftUI views, updated store observation.

**Tool:** `make ios-sim-fast`  
**Evidence:** `** BUILD SUCCEEDED **` — exit code 0. No compile errors in Swift or Rust.

---

#### VAL-IOS-DETECT-005: Codex-only flow unchanged (no picker shown)

**Behavioral description:** When connecting to a host where only Codex is detected, no agent picker sheet is presented. The connection progress UI shows the existing steps (ConnectingToSsh → FindingCodex → ... → Connected) without any agent selection step.

**Tool:** `make ios-sim-fast` + manual verification; `cargo test`.  
**Evidence:** Build succeeds. Code review confirms picker is only shown when `detected_agents.count > 1`. Existing Codex-only tests pass unchanged.

---

### Cross-Area Flows

#### VAL-CROSS-DET-001: Full flow — SSH connect → detect Pi+Droid+Codex → picker → select Pi → Pi session works

**Behavioral description:** End-to-end: user initiates SSH connect to a host with Pi, Droid, and Codex installed. After SSH auth, detection runs and finds all three. The agent picker appears on iOS. User selects Pi. The app disconnects the initial SSH probe session, reconnects with `AgentType::PiNative` (or best Pi transport), `create_provider_over_ssh()` spawns `pi --mode rpc`, the Pi transport connects, and the session shows Pi agent output.

**Tool:** Manual verification against gvps host with Pi + Droid + Codex installed.  
**Evidence:** iOS logs show: "detection complete: codex, pi (Native), droid (Native)" → "agent picker shown" → "user selected PiNative" → "provider transport connected" → session is active with Pi events.

---

#### VAL-CROSS-DET-002: Full flow — SSH connect → detect only Codex → skip picker → Codex bootstrap works

**Behavioral description:** End-to-end: user initiates SSH connect to a host with only Codex installed (no Pi or Droid). After SSH auth, detection runs and finds only Codex. No picker is shown. The guided flow proceeds through FindingCodex → StartingAppServer → Connected. The session works identically to the pre-detection behavior.

**Tool:** Manual verification against standard Codex host; `cargo test` regression suite.  
**Evidence:** iOS logs show: "detection complete: codex only" → "skipping agent picker" → "guided SSH connect proceeding" → normal Codex bootstrap succeeds.

---

#### VAL-CROSS-DET-003: Detection results are accurate against gvps host

**Behavioral description:** When running detection against the gvps development host, the results accurately reflect which agent binaries are installed. Pi detection finds `pi` at the correct path with the correct version. Droid detection finds or does not find `droid` based on the actual host state. Results match what `ssh <host> 'command -v pi; command -v droid'` returns when run manually.

**Tool:** Manual verification: run `detect_all_agents` via test binary against gvps; compare with manual SSH probe.  
**Evidence:** Detection output matches manual `command -v pi` / `command -v droid` results. Version strings match `pi --version` / `droid --version` output.

---

## Assertion Summary

| ID | Area | Title | Tool |
|----|------|-------|------|
| VAL-DETECT-001 | Detection Helper | Returns Codex when no Pi/Droid found | cargo test |
| VAL-DETECT-002 | Detection Helper | Returns Pi types when pi binary found | cargo test |
| VAL-DETECT-003 | Detection Helper | Returns Droid types when droid binary found | cargo test |
| VAL-DETECT-004 | Detection Helper | Returns all types when both Pi and Droid found | cargo test |
| VAL-DETECT-005 | Detection Helper | Runs Pi and Droid detection in parallel | code review |
| VAL-DETECT-006 | Detection Helper | Completes in <5 seconds | cargo test |
| VAL-GUIDED-001 | Guided SSH Connect | Detection runs after SSH auth, before Codex bootstrap | code review |
| VAL-GUIDED-002 | Guided SSH Connect | Only Codex found → normal Codex bootstrap unchanged | cargo test |
| VAL-GUIDED-003 | Guided SSH Connect | Multiple agents found → returns agent list for iOS picker | cargo test |
| VAL-GUIDED-004 | Guided SSH Connect | Detection failure falls through to Codex path | cargo test |
| VAL-GUIDED-005 | Guided SSH Connect | Existing Codex-only flow unchanged | cargo test + make ios-sim-fast |
| VAL-PROVIDER-001 | Provider SSH Connect | Validates selected agent exists before transport | cargo test |
| VAL-PROVIDER-002 | Provider SSH Connect | Returns clear error if agent not found | cargo test |
| VAL-PROVIDER-003 | Provider SSH Connect | Proceeds with provider factory when agent found | cargo test |
| VAL-IOS-DETECT-001 | iOS Detection Flow | iOS receives detected agent list when multiple found | code review + make ios-sim-fast |
| VAL-IOS-DETECT-002 | iOS Detection Flow | Agent picker sheet shown with detected agents | code review |
| VAL-IOS-DETECT-003 | iOS Detection Flow | Selecting agent reconnects with that agent type | code review |
| VAL-IOS-DETECT-004 | iOS Detection Flow | iOS builds with detection changes | make ios-sim-fast |
| VAL-IOS-DETECT-005 | iOS Detection Flow | Codex-only flow unchanged (no picker) | make ios-sim-fast + cargo test |
| VAL-CROSS-DET-001 | Cross-Area | Full flow: detect Pi+Droid+Codex → picker → Pi session | manual verification |
| VAL-CROSS-DET-002 | Cross-Area | Full flow: detect only Codex → skip picker → bootstrap | manual verification + cargo test |
| VAL-CROSS-DET-003 | Cross-Area | Detection results accurate against gvps host | manual verification |
