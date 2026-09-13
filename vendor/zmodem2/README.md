# zmodem2 0.7.2, local protocol fixes

Source: crates.io zmodem2 0.7.2, https://codeberg.org/jarkko/zmodem2.
The production Rust modules and both upstream licenses are retained. Upstream
examples, release tooling, executable integration harness and its build-time
program detection are not needed by the application and are omitted.

Local changes preserve the poll/submit API and add explicit sender start/skip
and acknowledged-position events, lrzsz ZFILE metadata, strict finish boundaries,
manual acceptance backpressure, and receiver OO completion. Application storage
must commit on FileCompleted before polling the queued ZRINIT acknowledgement.
Regression cases for these contracts live in src/runtime_tests.rs and in the
application's zmodem_runtime_tests.rs; real-peer acceptance belongs to the
application's PTY integration harness.
