# ue-is-renderer

**Unreal Engine as just a renderer. Possible?**

**Short answer: yes — as a *presentation layer*, not as a literal "renderer-only" library.**

Run the full UE 5.8 runtime, but move all *authority* — simulation, world composition, content
streaming decisions, and P2P networking — into an external **authoritative Rust** codebase. UE
actors/entities become **puppets driven each frame by Rust state**. This is essentially the shape
UE already assumes for a network client ("the server computes truth → the client renders an
approximation"), with Rust substituted for the server and a local transport for the replication
wire. **UE never sees the network** — each instance is fed only by its local Rust process, which
makes the P2P/authority concern cleanly separable from the engine.

Content (non-logic assets, levels, and **PCG**) is authored directly in the **Editor** or scripted
via the **MCP / Python / C++** APIs (editor-time content tooling) and **cooked** into a catalog the
Rust authority indexes into at runtime.

## Read the evaluation

📄 **[docs/evaluation.md](docs/evaluation.md)** — a full, cited feasibility evaluation (UE 5.8, as of
June 2026) covering:

- the decoupling spectrum and why "Level 2 — UE as presentation layer" is the realistic target,
  plus how low you can actually go (direct-`FScene` injection, `SceneViewExtension`, headless);
- subsystem-by-subsystem analysis: **rendering**, **world partition & streaming**, **networking /
  P2P / external authority**, **Mass Entity** (rendering many Rust-driven entities), the **Rust↔UE
  FFI/build** boundary, and **content authoring** (Editor, PCG, Python, Remote Control, MCP);
- precedent (NVIDIA ACE's UE renderer microservice; SpatialOS; digital twins/robotics);
- a **risk register**, a **division-of-responsibilities** table, and a **phased proof-of-concept
  plan** that de-risks the architecture milestone by milestone.

### The headline findings

| Area | Verdict |
|------|---------|
| Networking / P2P | ✓ **Cleanest fit.** UE runs Standalone and never touches the network; authority + P2P live entirely in Rust. |
| World streaming | ✓ Externally drivable via first-class C++ hooks (streaming sources, `SetDataLayerRuntimeState`) — but **content is baked at cook time** (Rust composes from a fixed catalog). |
| Rendering | ✓ Production-grade (Nanite, Lumen, VSM, Substrate, MegaLights) on Win/D3D12, Linux/Vulkan, PS5/Xbox. |
| Many entities | ✓ **Mass Entity** renders huge populations as instances (proven in LEGO Fortnite / Matrix Awakens) — but still experimental-tagged. |
| PCG / content | ✓ **PCG is production-ready (5.7+)**, runtime-capable; Python/MCP are *editor-time* tooling, not runtime. |
| Rust ↔ UE bridge | ⚠ Feasible but **you own it** — narrow C-ABI FFI; the UObject-GC ↔ Rust-ownership mismatch is the deepest trap; existing bridges (`uika`, `unreal-rust`) are pre-production. |
| Biggest costs | ⚠ Re-implementing client-side interpolation/prediction; the physics/animation authority split; maintaining the seam long-term. |

**Bottom line:** feasible and coherent for genres that tolerate the latency/animation model
(simulation, strategy, MMO-ish, co-op, visualization). It is a **bespoke integration you build and
maintain**, not a turnkey "renderer mode" — so prove it with the phased PoC before committing.

> Status: architecture/feasibility evaluation. No code yet; the repo is set up for a Rust-side
> authority (see `.gitignore`).
