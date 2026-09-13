# Release Agenterm

Agenterm publishes an Apple Silicon zip through GitHub Releases and updates the
`nickxudotme/homebrew-tap` cask from the same artifact.

## Prerequisites

- The Agenterm GitHub repository is public so Homebrew can download release artifacts.
- The repository secret `TAP_DEPLOY_KEY` contains the private half of a write-enabled deploy key
  on `nickxudotme/homebrew-tap`.
- The cask exists at `Casks/agenterm.rb` in the tap repository.

The deploy key is optional for creating a GitHub Release. Without it, the workflow leaves the
release intact and reports that the cask must be updated manually.

## Release

Push a semantic version tag:

```sh
git tag v0.1.0
git push origin v0.1.0
```

The `Release` workflow can also be started manually with a version such as `0.1.0`. It builds
`Agenterm-0.1.0-arm64.zip`, verifies the bundle metadata and ad-hoc signature, publishes the zip
and checksum, then updates the cask version and SHA-256.

## Local Artifact

```sh
./script/build_macos_release 0.1.0
```

The artifact and checksum are written to `dist/`.

## Signing

The current artifact is ad-hoc signed and not notarized. Gatekeeper may require users to remove
the quarantine attribute after installation:

```sh
xattr -dr com.apple.quarantine /Applications/Agenterm.app
```
