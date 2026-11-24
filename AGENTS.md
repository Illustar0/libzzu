# AGENTS.md

## Overview

This repository implements the campus services SDK for a University, providing unified client access to campus-wide integrated authentication (CAS & MFA), undergraduate academic affairs and timetables (`eas`), campus e-card and dorm electricity services (`ecard`), campus email (`mail`), and campus network portal & self-service (`network`).

## Workspace Structure

- `crates/zzu` — Async Rust client API and the shared protocol logic, session state, cryptography, and data models behind it (`#![forbid(unsafe_code)]`).
- `crates/libzzu` — C ABI export layer (`cdylib` / `staticlib`) with `cbindgen` header generation at `crates/libzzu/include/libzzu.h`. Language SDKs bind to this ABI; its contract lives in `crates/libzzu/ABI.md`.

## Common Commands

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

## Guidelines

- **Safety boundaries**: `zzu` forbids `unsafe` code. All raw-pointer FFI glue belongs in `libzzu`, and every `unsafe` block must carry a `// SAFETY:` comment (`undocumented_unsafe_blocks = "deny"`).
- **C header sync**: When changing `libzzu` public exports or doc comments, update `crates/libzzu/include/libzzu.h` to match `cbindgen` output (enforced by `cargo test -p libzzu`).
- **ABI shape**: Each `zzu` client maps one-to-one to an opaque handle with `new`/`free`/`call`/`call_blocking`, and handles keep `zzu`'s construction relationships; never merge clients into an aggregate. Add client I/O capabilities as JSON methods under `crates/libzzu/src/methods`, not as new exports. Export local pure functions on data (such as `zzu_timetable_to_calendar`) individually instead of attaching them to a client. Record both in `ABI.md`. Exported signatures use only fixed-width integers, `size_t`, pointers, and `ZzuCallback`; no struct crosses the boundary.
- **Session lifecycle**: Protocol state machines, token validity checks, and serialization of concurrent calls stay in `zzu`. Persistence and the timing of re-login belong to SDKs. `zzu` must not spawn background tasks or use the stored password implicitly; surface `token_expired` instead.
- **ABI versioning**: New methods or fields bump `ZZU_ABI_VERSION_MINOR`. Changing an export, removing or renaming a method or field, or changing a field's meaning bumps `ZZU_ABI_VERSION_MAJOR`.
- **Documentation**: Workspace lints enable `missing_docs = "warn"` and `clippy::pedantic = "warn"`; document all public items clearly and concisely.
