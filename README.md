# omni-utils

Small Rust utilities for NEAR smart contracts plus a derive macro crate.


> [!NOTE]
> In the future, this should be part of `near-plugins`.

## Contents

- `omni-utils`: helpers like `NearExpect` and `PromiseOrPromiseIndexOrValue`, plus a re-export of `ErrorDisplay`.
- `omni-utils-derive`: proc-macro derive for formatting enum errors (expects the enum to implement `AsRef<str>`).

## Quick usage

```rust
use omni_utils::{ErrorDisplay, near_expect::NearExpect, promise::PromiseOrPromiseIndexOrValue};

#[derive(ErrorDisplay, strum::AsRefStr)]
enum MyError {
    BadInput(String),
}

fn handler(opt: Option<u64>) {
    let value = opt.near_expect("ERR_MISSING");
    PromiseOrPromiseIndexOrValue::Value(value).as_return();
}
```
