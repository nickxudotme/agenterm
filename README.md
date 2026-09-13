# Agenterm

Agenterm is a local-first macOS terminal built from the Warp OSS terminal stack.

## Install with Homebrew

Agenterm currently supports Apple Silicon Macs:

```bash
brew install --cask nickxudotme/tap/agenterm
```

If macOS blocks the first launch, remove the quarantine attribute and open Agenterm again:

```bash
xattr -dr com.apple.quarantine /Applications/Agenterm.app
```

The current baseline provides a clean local shell, terminal blocks, scrolling,
selection, copy and paste, and tabs. It deliberately does not start or expose
Warp cloud sync, sign-in, Agent Mode, directory trees, Drive, or the MCP client.

## Run locally

```bash
cargo run -p warp --bin agenterm
```

The app stores its data under its own Agenterm application identifier and does
not import the installed Warp application's configuration.

## Direction

The codebase is being reduced in buildable steps around the terminal and WarpUI
foundations. Planned product work includes nested warpify and an MCP server for
external callers; neither is implemented in this baseline.

## License

`crates/warpui_core` and `crates/warpui` retain their MIT license. The remaining
code retains the upstream AGPL v3 license. See [LICENSE-MIT](LICENSE-MIT) and
[LICENSE-AGPL](LICENSE-AGPL).
