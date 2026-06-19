# Proof-of-concept scaffold

This turns the [evaluation](evaluation.md) into an **actionable, validated
foundation**. It locks down the highest-leverage artifact — the **Rust ↔ UE
interface** everything else depends on — as real, compiling, tested code, and
specifies the UE side as a contract.

## What's here

| Crate / dir | Role | State |
|---|---|---|
| [`crates/ue-renderer-protocol`](../crates/ue-renderer-protocol) | The state/wire contract: `WorldSnapshot` (Rust→UE), `PlayerInput` (UE→Rust), transforms, streaming directives, interpolation. Zero deps, explicit little-endian format, `#![forbid(unsafe_code)]`. | ✅ built + tested |
| [`crates/ue-authority`](../crates/ue-authority) | Example authoritative sim: fixed-timestep, deterministic, consumes input, emits snapshots. Stand-in for the real game logic + P2P. | ✅ built + tested |
| [`crates/ue-bridge-ffi`](../crates/ue-bridge-ffi) | The narrow **C ABI** UE links against (`cdylib`+`staticlib`): create/tick/pull-latest-snapshot/push-input, panic-isolated, latest-wins. | ✅ built + tested |
| [`bridge/include/ue_bridge.h`](../bridge/include/ue_bridge.h) | C header for the UE C++ plugin (kept in sync with the crate). | ✅ |
| [`bridge/README.md`](../bridge/README.md) | The **UE-side integration contract** (the C++ adapter spec). | 📄 contract (needs UE 5.8 to build) |

## What is and isn't proven here

- **Proven (runs in CI / locally):** the wire contract round-trips; interpolation
  matches entities by id across ticks; the sim is deterministic; the C-ABI
  lifecycle works end-to-end (create → push input → tick → pull → decode →
  destroy); the `cdylib`/`staticlib` build and export unmangled C symbols; clippy
  is clean.
- **Not proven here (requires a licensed UE 5.8 install):** the C++ adapter,
  rendering, World Partition streaming control, Mass/`FScene` representation, and
  end-to-end latency. Those are specified in [`bridge/README.md`](../bridge/README.md)
  and remain the subject of the evaluation's phased plan.

## How it maps to the evaluation's phased PoC (§5)

This scaffold delivers the Rust foundation for **Milestone 1** ("UE Standalone
renders N proxies driven by a local Rust process, with interpolation") and the
contract for **Milestone 2** (external streaming control via the
`StreamingState` directives) and **Milestone 6** (swap the hot-path transport —
the C ABI already isolates that choice). The `anim_state` field anticipates
**Milestone 3**; the representation tiers in `bridge/README.md` anticipate
**Milestone 4** (Mass).

## Build & test

```sh
cargo test --workspace          # 11 tests across the three crates
cargo clippy --workspace --all-targets
cargo build -p ue-bridge-ffi --release   # -> target/release/{libue_bridge_ffi.so,.a}
```

No network or external crates are required.

## Next steps

1. Stand up the UE 5.8 C++ adapter from [`bridge/README.md`](../bridge/README.md)
   and render the example sim's entities as proxies with interpolation.
2. Replace `ue-authority` with the real authoritative simulation + P2P transport
   (Rust↔Rust over the wire; UE keeps talking only to the local bridge).
3. Profile end-to-end latency and, if needed, move the hot path from the
   synchronous tick to a lock-free shared-memory slot behind the same C ABI.
