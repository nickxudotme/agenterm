# ZMODEM Implementation Tasks

Source: `docs/design-docs/terminal/zmodem-rzsz/spec.md`. User authorized autonomous implementation;
independent spec review must close before code dispatch. No commits or pushes.

## Current State

- Current: independent design review PASS; T1 and T2 dispatched.
- T1/T2 are additive; T3 migrates production routing; T4 migrates GUI; T5 completes UI/cross-transfer.
- Build/test/review commands belong to coordinator, not implementers.

## Dependencies And Parallel Groups

- PG-A: T1, T2, T4A settings foundation (disjoint files).
- PG-B: T3 after T1/T2; T4 settings work may begin after core message contract settled.
- T5 after T3/T4; T6 after T3 with separate test-only write set; T7 after all.

## Tasks

### T1 [~] Native Protocol And Worker

- Files: new `crates/warp_terminal/src/zmodem_runtime.rs` and test file, `vendor/zmodem2/`,
  `crates/warp_terminal/src/zmodem.rs`, `crates/warp_terminal/Cargo.toml`, root Cargo.toml patch only.
- Context: old zmodem.rs Session/Detector, zmodem2 0.7.2 Action and Sender/Receiver;
  consumers local_tty/event_loop.rs and app/src/terminal/view.rs. New module initially registered
  from zmodem.rs so no conflict in lib.rs. Keep old APIs compile until migration.
- Implement bounded cancelable per-transfer worker, ordered metadata events, upload reads by offset,
  download safe temporary-file persistence, real completion, canonical abort, timeout and finish tails.
  Provide public transfer command/event types and nonblocking PTY adapter API. Vendor minimal upstream
  core only when required; preserve licenses. Native protocol assumptions need real-peer regressions.
- Spec: 3.1/3.2, 4.2-4.5.4, 6.2-6.6, 7.
- Acceptance:
  - [ ] Core builds, no full-file buffers or UI disk I/O in new runtime.
  - [ ] Worker tests cover cancellation, bounded queues, file safety and ordered events.
  - [ ] Real sz/rz both directions incl empty/multi/binary finish normally and hash match.

### T2 [~] Detector Correctness

- Files: new `crates/warp_terminal/src/zmodem_detector.rs` and `_tests.rs`, module registration in lib.rs.
- Context: old zmodem.rs detector/headers, local_tty/event_loop.rs route_zmodem.
- Implement streaming detection of CRC-valid ZRQINIT/ZRINIT/ZFILE only, exact preserved bytes,
  bounded pending prefix, explicit expiration/flush; no swallowing unsupported headers.
  Keep isolated and avoid changes to old zmodem.rs so T1 is disjoint.
- Spec: 3.1, 4.5.1, 6.4, 7.
- Acceptance:
  - [ ] Unit tests pass for arbitrary splits, non-start headers, normal trailing stars and expiry.
  - [ ] Module builds and non-protocol roundtrip preserves exact bytes.

### T3 [~] PTY Routing And Message Contract

- Files: local_tty/event_loop.rs, local_tty/mod.rs, local_tty/unix.rs,
  writeable_pty/message.rs, event.rs.
- Depends: T1/T2.
- Context: Message input path, State write queue, mio_channel, ActiveTerminal locks,
  ChannelEventListener; new runtime and detector API.
- Add new typed Zmodem control/event while retaining old variants until app migration.
  Replace old routing with bounded worker transport, independent deadlines, input exclusion,
  cancellation priority, output remainder return, disabled-by-default capability activated by GUI.
  Remove unbounded content trace and incorrect force-enable termios behavior.
- Acceptance:
  - [ ] Core Clippy/build/tests pass; no I/O under terminal model lock except PTY itself.
  - [ ] Production routing tests cover silent timeout, normal tails, queue full and cancellation.

### T4A [~] Preferences Foundation

- Files: new app/src/terminal/zmodem_settings.rs, terminal/mod.rs registration, settings/init.rs,
  settings_view/warpify_page.rs only. No view.rs/action.rs/init.rs edits in parallel.
- No dependency on runtime; policy enums mapped by GUI integration later.
- Add local settings and visible controls for enable, ask-directory, directory, overwrite policy,
  drag mode, upload command and cross-transfer enable. Command-palette binding belongs to T4.
- Acceptance:
  - [ ] GUI build and settings tests pass, all settings local-only and visible.

### T4 [~] GUI Lifecycle And Preferences

- Files: app terminal zmodem_transfer.rs/tests, view.rs/init.rs/action.rs, event.rs/model_events.rs,
  PtyController/terminal_surface/terminal_manager_util, remote_tty/event_loop.rs,
  app/lib.rs, app_menus.rs, bindings.rs; new settings module and existing settings panel registration.
- Depends: T3 public contract and T4A.
- Context: existing file pickers, settings macros and ssh_file_upload view; per-view lifetime and
  TerminalManager subscriptions. Events delivered to correct view, no global file content state.
- Native directory picker before downloads, single multi-picker for rz, callbacks keyed by ID,
  cancel UI/menu/Ctrl-C, settings and command-palette discovery, status including skipped results.
  Opt-in ZMODEM drag path sends configured rz only to bound current terminal.
- Acceptance:
  - [ ] GUI builds, settings retained locally and actions are reachable.
  - [ ] No GUI pending file bytes; late dialog replies harmless; parallel transfers isolated.
  - [ ] Manual GUI picker/menu/cancel/settings and drag verification.

### T5 [ ] Cross-Terminal Transfer

- Files: app ZMODEM coordinator and target picker/UI, relevant terminal lifecycle hooks.
- Depends: T4.
- Context: GUI ownership and runtime control API, live TerminalView discovery.
- Explicit opt-in source-target reservation, temporary private staging, phased statuses,
  target validity checks, linked cancel, reliable cleanup. No focus-based routing.
- Acceptance:
  - [ ] Two-stage transfer hashes match destination; wrong/closed/busy target rejected.
  - [ ] Source/target cancel and close clean up, third terminal unaffected.

### T6 [~] Production Transport Regression Harness

- Files: crates/warp_terminal/tests/zmodem_lrzsz.rs and new test-only helpers, local shell fixtures.
- Depends: T3; disjoint from T4/T5.
- Context: EventLoop/ActiveTerminal/EventedPty and runtime APIs, existing lrzsz tests.
- Drive production routing through actual PTY, bound every wait/write and ensure Drop cleanup.
  Require peer success exit and prompt recovery; cover multi/binary/empty/name/conflict/cancel,
  fragments and silent peers. Tests must fail explicitly if required test tools absent in acceptance.
- Acceptance:
  - [ ] All real-peer regressions pass and no success test kills peer to hide failure.
  - [ ] 256 MiB each direction, SHA-256, throughput/RSS evidence and bounded-memory scaling.

### T7 [ ] Final Verification And Cleanup

- Depends: all prior tasks. Coordinator verification, follow-up code diffs via relevant implementers.
- Remove obsolete APIs only after migrated consumers build. Run format, three presubmit Clippy legs,
  affected nextest, GUI build/run and local SSH flows; independent Standard code review.
- Acceptance:
  - [ ] Checks passing or precise pre-existing blockers documented without success claims.
  - [ ] GUI screenshots and real interaction proof; local SSH/multihop results.
  - [ ] Review issues resolved, artifacts truthful, runnable local build handed over.

## Coverage

| Requirement | Tasks |
| --- | --- |
| 3.1 rz/sz/file policies/cancel | T1,T3,T4,T6 |
| 3.1 drag/preferences/cross transfer | T4,T5 |
| 3.2 correctness/resources/timers | T1,T2,T3,T6 |
| 3.3 deployment/platform constraints | T3,T4,T7 |
| 4.1-4.4 ownership/contract | T1,T3,T4 |
| 4.5.1-4.5.4 mechanism | T1,T2,T3 |
| 4.5.5 staging | T5 |
| 6 compatibility/concurrency/security | T1-T7 |
| 7 verification/8 risks | T6,T7 |
