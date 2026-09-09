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

For strict enterprise access, prefer ordinary SSH by default and make basic
SSH Warpify a per-host opt-in. `ssh -J bastion target` is preferable to an
interactive `ssh bastion` followed by another SSH: the current wrapper does
not recursively bootstrap an SSH session launched from an already remote
shell.

## Suggested next steps

1. Make SSH Warpify policy explicit in Agenterm: default off, per-host opt-in,
   remote-server extension hard-disabled.
2. Design the desired recursive/nested Warpify protocol independently of the
   upstream SSH wrapper.
3. Continue physical dependency reduction only after retaining test coverage
   for the local terminal, shell bootstrap, ANSI parser, and Warp blocks.
4. Add the future external MCP server as a separate local-only subsystem; do
   not re-enable the upstream MCP client surface.
