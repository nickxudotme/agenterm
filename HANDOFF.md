# Agenterm handoff

This document captures the useful outcome of the initial Agenterm development
session so work can continue from another machine or conversation.

## Current baseline

`v0.1.0-terminal-baseline` points to commit `cb581af`.

Agenterm is a local-first macOS terminal derived from Warp OSS. The current
product target is deliberately narrow: a native local terminal with Warp's
terminal rendering and command blocks, while upstream account, cloud, AI,
Agent, Drive, directory-tree, MCP-client, onboarding, telemetry, and related
UI entry points are disabled or hidden.

Build and run:

```sh
cargo check -p warp --bin agenterm
./script/run
```

The development bundle is produced at
`target/debug/bundle/osx/Agenterm.app`.

## Behaviour verified during the session

- Agenterm launches into a local shell without onboarding or login.
- It uses its own app identifier and persistence location; it does not import
  the installed official Warp application's settings.
- Settings opens from Appearance and `Cmd-,`.
- Local terminal shortcuts were restored: `Cmd-N`, `Cmd-T`, and `Cmd-W`.
- The debug-only per-block `Lines` / `Size` information is hidden for the OSS
  channel.
- AI-related entries were filtered from the keyboard-shortcut settings view.
- Cloud listener startup was disabled for the OSS channel.

The source still contains much of the upstream Warp implementation. This is a
functional, stable *product-surface* reduction rather than a completed
physical deletion of every unrelated crate.

## Key commits

- `6d411b0` establish Agenterm terminal baseline
- `1a987fa` disable cloud listener in Agenterm
- `45f675a` replace Warp project entrypoints with Agenterm
- `97f6512` retain only local terminal menus
- `780f581` remove unused cloud launch remnants
- `28a425e` open Agenterm settings from appearance
- `60175c9` restore tab close shortcut and hide NLD debug text
- `107e2de` restore local terminal tab shortcuts
- `cb581af` limit Agenterm keybindings to local terminal actions

## Warpify findings

Local Warpify is an in-band nested-shell protocol: it injects a bootstrap into
the same PTY, then recognizes terminal control-sequence hooks emitted by the
nested shell. Relevant files include:

- `app/src/terminal/view.rs`
- `app/src/terminal/warpify/settings.rs`
- `app/src/terminal/bootstrap.rs`
- `crates/warp_terminal/src/bootstrap.rs`
- `app/src/terminal/model/terminal_model.rs`
- `app/src/terminal/warpify/trigger_state.rs`

SSH Warpify does **not** currently require tmux. Its basic mode is an OpenSSH
ControlMaster wrapper that injects a remote bootstrap. It requires a remote
PTY, permission to execute a supplied remote command, and a compatible remote
`bash` or `zsh` environment. It is not a pure-POSIX, transparent connection.

The optional remote-server extension is inappropriate for strict jump hosts:
it writes and executes a binary under `~/.warp-dev/remote-server` for the OSS
channel, needs write/execute permission plus archive tools and either
`curl`/`wget` or an SCP fallback, and Linux binaries require glibc 2.31 or
newer. The upstream source itself still has a TODO for how that extension
should work on the OSS channel. Keep it disabled in Agenterm.

For strict enterprise access, preserve ordinary SSH and company authentication.
Do not assume ProxyJump works: some bastions prohibit forwarding. The current
wrapper does not recursively bootstrap an SSH session launched from an already
remote shell.

## September 10 investigation checkpoint

### Changes in this checkpoint

- Expose the Warpify settings page in the OSS navigation.
- Route Edit > Paste through `CustomAction::Paste`, like the working terminal
  context menu. The previous native `paste:` menu action had no receiver in the
  terminal window. A regression test checks the custom item and shortcut.
- Add temporary warning-level diagnostics for in-band command cancellation,
  failed result IDs/exit codes/output sizes, and failed directory-listing paths.
  These diagnostics do not add full command bodies or output contents to logs.
  Remove or lower their verbosity after resolving the issue.

### Verified and rejected hypotheses

- Fixed-byte DCS and OSC probes arrived byte-for-byte intact. Earlier claims of
  filtering/truncation were based on incorrectly quoted probes and are withdrawn.
- The restricted bastion lacks bootstrap utilities by policy. Do not install
  substitutes or evade the whitelist. This route remains unsupported.
- Direct interactive SSH to the final target works. A constructed noninteractive
  nested SSH invocation failed authentication; that did not mean the user's normal
  login was broken. Preserve the configured interactive route.
- On the final target, a disposable Bash 4.2 child running the real repository
  bootstrap emitted Bootstrapped, Preexec, CommandFinished and Precmd. Commands
  with exit codes 0 and 1 were reported correctly. No RC files were modified.
- Isolated directory listing and generator encoding succeeded on that target.
- Hosts with a configured RemoteCommand fall back to plain SSH. Automatic
  Warpification of the final target has NOT been implemented.
- Manual integration can use the existing SourcedRcFileForWarp hook from an
  already authenticated Bash prompt. Do not synthesize Bootstrapped or use an
  InitSubshell hook without a client-registered session ID.

### Remaining failure and next investigation

The user observed the integrated UI after manual initialization, but background
completion still fails. New diagnostics show the remote directory listing is
given the preceding local macOS home path and exits with code 1. This is a
cross-session context problem, not evidence that the target lacks `find`.

Cancellation logs also explain a mismatched result: a consumer cancelled its
request before the old remote result arrived. This is not yet proven to cause
the directory failure; do not change scheduling based on that warning alone.

Inspect `BlockList::apply_precmd_to_active` in `app/src/terminal/model/blocks.rs`:
in-band prompts reuse `last_populated_precmd_payload` without checking session
identity. This is a candidate, NOT a verified root cause. Trace initial remote
Precmd delivery and Input's active block metadata before fixing it. Add a
local-to-remote transition regression test, then verify command blocks, paths,
completion and exit behavior in the actual app before adding automatic entry.

### Verification and delivery status

- `cargo check -p warp --bin agenterm --features gui`: passed.
- Menu regression: 1 passed; existing in-band executor tests: 9 passed.
- `./script/run --dont-open`: build, bundle and signing passed.
- Repository format check passed (including one pre-existing import-order fix).
- Full presubmit Clippy was not completed. The first workspace attempt required
  Yarn 4.0.1; after installing the pinned JS dependencies, checks were restarted
  and then interrupted at the user's request to push this checkpoint immediately.
- The new local app was started. Native menu interaction still needs user
  confirmation; do not equate the structural menu test with GUI verification.
- The SSH/remote completion task is incomplete. Do not report this checkpoint
  as a completed automatic Warpify fix.

## Suggested next steps

1. Make SSH Warpify policy explicit in Agenterm: default off, per-host opt-in,
   remote-server extension hard-disabled.
2. Design the desired recursive/nested Warpify protocol independently of the
   upstream SSH wrapper.
3. Continue physical dependency reduction only after retaining test coverage
   for the local terminal, shell bootstrap, ANSI parser, and Warp blocks.
4. Add the future external MCP server as a separate local-only subsystem; do
   not re-enable the upstream MCP client surface.
