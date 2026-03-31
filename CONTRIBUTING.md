# Contributing to vzglyd-sidecar

Thanks for your interest in contributing.

## What this crate is

`vzglyd-sidecar` provides networking and IPC utilities for vzglyd slide sidecars. All code must compile for the `wasm32-wasip1` target.

## Development

```bash
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
