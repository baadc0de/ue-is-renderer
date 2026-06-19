# UE renderer adapter — integration contract

This directory specifies the **Unreal Engine 5.8 side** of the boundary: a thin
C++ plugin (the "adapter") that links the [`ue-bridge-ffi`](../crates/ue-bridge-ffi)
library and drives UE as a renderer from the authoritative Rust core.

> The Rust side in this repo is real and tested (`cargo test`). The C++ adapter
> below is a **contract/spec**, not committed code — it requires a licensed UE
> 5.8 installation to compile and cannot be built or run in this repository.
> It is the concrete realization of `docs/evaluation.md` (esp. §2.B, §2.C, §2.E,
> §1.3, and the §3 responsibilities table).

## What the adapter does

Each UE **game-thread** tick, the adapter:

1. `ue_bridge_tick(handle, DeltaSeconds)` — advance the authority (or, in a
   threaded design, this is a no-op read; see "Threading").
2. `ue_bridge_latest_snapshot(handle, Buf, Cap, &Len)` — pull the latest encoded
   `WorldSnapshot` into a **reused** buffer (grow only on `UE_ERR_BUFFER_TOO_SMALL`).
   This is the *latest-wins* hand-off: the game thread never blocks.
3. **Decode** the bytes (mirror the format in
   [`crates/ue-renderer-protocol`](../crates/ue-renderer-protocol/src/lib.rs)).
4. **Apply** the snapshot to the scene (see "Mapping" below), keeping the
   previous snapshot so it can **interpolate** between the two for display.
5. Gather local input and `ue_bridge_push_input(handle, Bytes, Len)` (UE → Rust).

```cpp
// Pseudocode — runs on the game thread (e.g. from a UGameInstanceSubsystem tick).
void FUeBridgeRunner::Tick(float DeltaSeconds)
{
    if (ue_bridge_tick(Handle, DeltaSeconds) != UE_OK) { /* log, bail */ return; }

    size_t Len = 0;
    int32 Rc = ue_bridge_latest_snapshot(Handle, Buf.GetData(), Buf.Num(), &Len);
    if (Rc == UE_ERR_BUFFER_TOO_SMALL) { Buf.SetNumUninitialized(Len);
        Rc = ue_bridge_latest_snapshot(Handle, Buf.GetData(), Buf.Num(), &Len); }
    if (Rc != UE_OK) { return; }

    Prev = Curr;                       // keep last snapshot for interpolation
    Curr = DecodeSnapshot(Buf, Len);   // mirror of ue-renderer-protocol
    ApplySnapshot(Curr);               // spawn/despawn, update proxies, streaming

    const FInputBytes In = EncodeLocalInput();
    ue_bridge_push_input(Handle, In.Ptr, In.Len);
}

// On render/each frame, interpolate Prev->Curr by (timeSinceCurr / tickInterval).
```

## Mapping a snapshot onto UE

| Snapshot field | UE action |
|---|---|
| `Event::Spawn{id, archetype}` | Create a proxy for `id` using catalog entry `archetype` (a cooked mesh/skeleton). Pick representation per "Representation tiers" below. |
| `Event::Despawn{id}` | Destroy the proxy for `id`. |
| `EntityState.transform` / `.velocity` | Set the proxy transform; use velocity to **extrapolate** the local player and dead-reckon remotes between ticks. |
| `EntityState.anim_state` / `.anim_play_rate` | Drive the proxy's **AnimBP** (state id → state machine; play rate). UE owns the actual poses/blending. |
| `EntityState.flags` (`FLAG_LOCAL_PLAYER`) | Mark the local player (drive camera; prefer local prediction over interpolation). |
| `Event::OneShot{cue, position}` | Trigger a Niagara/audio cue at `position`. |
| `StreamingSource{position, radius, target, priority}` | Register/update a `UWorldPartitionStreamingSourceComponent` (or a custom `IWorldPartitionStreamingSourceProvider`) on `UWorldPartitionSubsystem`; set `TargetState` (Loaded/Activated) and `Priority`. Keep `APlayerController::bEnableStreamingSource=false` so only Rust drives streaming. |
| `DataLayerState{name, state}` | `UDataLayerManager::SetDataLayerRuntimeState(asset, state)`. |
| (gating) | Before showing newly-streamed content, poll `UWorldPartitionSubsystem::IsStreamingCompleted(...)`. |

### Representation tiers (per `docs/evaluation.md` §1.3 / §2.D)

- **Many similar entities** → **Mass Entity** + MassRepresentation (ISM). Write
  `transform`/`velocity` into Mass fragments via deferred commands.
- **Bespoke / rapidly-changing geometry** → push directly into `FScene`
  (`AddPrimitive` / `UpdatePrimitiveTransform`, or a custom `FPrimitiveSceneProxy`
  with `GetDynamicMeshElements`) — bypasses the actor tick. Needs the `RenderCore`
  module.
- **A few near-camera, high-fidelity entities** → full `AActor` proxies with the
  complete AnimBP/physics/VFX stack.

## Wire format (mirror this in C++)

Little-endian. Sequences are `u32 count` then elements. Strings are `u32 len`
then UTF-8 bytes. A framed message is `magic[4] | version:u16 | flags:u16 | body`.

- `WorldSnapshot` magic `"UERS"`, `PlayerInput` magic `"UEPI"`, `version = 1`.
- `WorldSnapshot` body: `tick:u64, sim_time_s:f64, entities:[EntityState],
  events:[Event], streaming:StreamingState`.
- `EntityState`: `id:u64, archetype:u32, flags:u32, transform, velocity:Vec3,
  anim_state:u16, anim_play_rate:f32`.
- `Transform`: `translation:Vec3, rotation:Quat(x,y,z,w), scale:Vec3`; `Vec3` = 3×f32.
- `Event`: `tag:u8` then `0`→`Spawn{id:u64, archetype:u32}`, `1`→`Despawn{id:u64}`,
  `2`→`OneShot{cue:u32, position:Vec3}`.
- `StreamingState`: `sources:[StreamingSource], data_layers:[DataLayerState]`.
- `StreamingSource`: `position:Vec3, radius:f32, grid:u32, target:u8(0=Loaded,
  1=Activated), priority:u8`.
- `DataLayerState`: `name:String, state:u8(0=Unloaded,1=Loaded,2=Activated)`.
- `PlayerInput` body: `move_dir:Vec3, yaw:f32, pitch:f32, buttons:u32`.

The Rust source in `crates/ue-renderer-protocol/src/lib.rs` is authoritative;
its tests pin the exact byte layout.

## Linking into UE (`*.Build.cs`)

The Rust crate builds a `cdylib` (`.dll`/`.so`, hot-reloadable) and a `staticlib`
(`.lib`/`.a`). Integrate as an `External` module dependency:

```csharp
// UnrealBuildTool cannot run cargo; build Rust in a pre-build step, then:
PublicIncludePaths.Add(Path.Combine(ModuleDirectory, "../../bridge/include"));
PublicAdditionalLibraries.Add(/* path to ue_bridge_ffi.lib / libue_bridge_ffi.a */);
RuntimeDependencies.Add(/* stage ue_bridge_ffi.dll / .so next to the binary */);
// Add std's native deps via PublicSystemLibraries (discover with:
//   cargo rustc -p ue-bridge-ffi -- --print=native-static-libs)
```

### Build pitfalls (from `docs/evaluation.md` §2.E)

- **Build Rust `--release`** and keep both sides on the **release dynamic CRT**
  (Windows `LNK2038` otherwise). Match the **MSVC toolset** to your UE version.
- **Panic isolation** is already handled (every entry point uses `catch_unwind`
  → `UE_ERR_PANIC`); never let Rust unwind into UE.
- **No `UObject` crosses the boundary** — only bytes/primitives — so the
  UObject-GC ↔ Rust-ownership trap (R4) does not arise here. If you later expose
  UObjects to Rust, you must pin/unpin GC roots explicitly.
- Check `ue_bridge_abi_version() == UE_BRIDGE_ABI_VERSION` at load.

## Threading

This reference treats the bridge as synchronous on the game thread. To run the
Rust authority (and P2P) on its own thread/process, replace `ue_bridge_tick`'s
body with a write to a lock-free latest-snapshot slot and have
`ue_bridge_latest_snapshot` read it; the C ABI is unchanged. All `UObject`/scene
mutations must still happen on the **game thread**.

## Networking note

UE runs **Standalone** and never opens a `NetDriver`. All networking — including
P2P and authority — lives in Rust; UE is fed only by the **local** bridge. See
`docs/evaluation.md` §2.C.
