# Contributing to VRX-64-sidecar

Thanks for your interest in contributing.

## What this crate is

`VRX-64-sidecar` provides networking and IPC utilities for vzglyd slide sidecars. Changes here affect every sidecar that relies on the VZGLYD channel and socket ABI, so keep the surface small and deliberate.

## Development

```bash
cargo test
cargo check --target wasm32-wasip1
cargo clippy -- -D warnings
cargo fmt
```

## Pull requests

- Keep PRs focused — one concern per PR
- Update CHANGELOG.md under `[Unreleased]`
- All public API additions must have doc comments
- Do not add dependencies that don't support `wasm32-wasip1`

## Code of conduct

This project follows the [Contributor Covenant](CODE_OF_CONDUCT.md).
