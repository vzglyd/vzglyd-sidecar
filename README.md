# `VRX-64-sidecar`

`VRX-64-sidecar` is the standard library for VZGLYD sidecars: small `wasm32-wasip1` programs that fetch live data and push it to a paired slide.

Add it to a sidecar crate:

```toml
[dependencies]
VRX-64-sidecar = { git = "https://github.com/vzglyd/VRX-64-sidecar" }
```

Typical usage:

```rust
use vzglyd_sidecar::{https_get_text, poll_loop};

fn main() {
    poll_loop(300, || {
        let body = https_get_text("api.example.com", "/forecast")?;
        Ok(body.into_bytes())
    });
}
```

This crate is intended for the `wasm32-wasip1` target used by VZGLYD sidecars.

Further reading:

- [Slide authoring guide](https://github.com/vzglyd/vzglyd/blob/main/docs/authoring-guide.md)
- [VRX-64-sidecar repository](https://github.com/vzglyd/VRX-64-sidecar)
- [VZGLYD repository](https://github.com/vzglyd/vzglyd)
