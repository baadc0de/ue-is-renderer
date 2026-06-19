//! # ue-authority
//!
//! A tiny, deterministic stand-in for the authoritative game simulation. Its job
//! here is **not** to be a real game — it is to demonstrate the half of the
//! architecture that owns truth: it ticks at a fixed timestep, consumes
//! [`PlayerInput`], and emits a [`WorldSnapshot`] each tick for the UE renderer
//! to display (see `docs/evaluation.md` §1 and `bridge/README.md`).
//!
//! It is intentionally dependency-free and deterministic: given the same inputs
//! and timesteps it produces byte-identical snapshots, which is the property a
//! deterministic-lockstep/rollback P2P layer would rely on (§2.C).

use ue_renderer_protocol::{
    DataLayerRuntimeState, DataLayerState, EntityState, Event, PlayerInput, StreamingSource,
    StreamingState, StreamingTargetState, Transform, Vec3, WorldSnapshot, FLAG_LOCAL_PLAYER,
    FLAG_VISIBLE,
};

/// Number of orbiting "AI" agents the example spawns.
const NUM_AGENTS: u64 = 8;
/// Player movement speed in cm/s.
const PLAYER_SPEED: f32 = 600.0;
/// Streaming-source activation radius around the player (cm).
const STREAM_RADIUS: f32 = 12_000.0;

/// The authoritative world.
pub struct Sim {
    tick: u64,
    time_s: f64,
    player_pos: Vec3,
    player_vel: Vec3,
    pending_input: PlayerInput,
    /// Set once on the first step so the renderer gets Spawn events.
    spawned: bool,
}

impl Default for Sim {
    fn default() -> Self {
        Self::new()
    }
}

impl Sim {
    pub fn new() -> Self {
        Self {
            tick: 0,
            time_s: 0.0,
            player_pos: Vec3::ZERO,
            player_vel: Vec3::ZERO,
            pending_input: PlayerInput::default(),
            spawned: false,
        }
    }

    /// Queue the latest local player's intent (UE → Rust). Last write wins per tick.
    pub fn apply_input(&mut self, input: PlayerInput) {
        self.pending_input = input;
    }

    /// Advance the simulation by `dt` seconds.
    pub fn step(&mut self, dt: f64) {
        let dt32 = dt as f32;
        // Integrate the player from the most recent input.
        let dir = self.pending_input.move_dir;
        let len = (dir.x * dir.x + dir.y * dir.y + dir.z * dir.z).sqrt();
        self.player_vel = if len > 1e-6 {
            Vec3::new(
                dir.x / len * PLAYER_SPEED,
                dir.y / len * PLAYER_SPEED,
                dir.z / len * PLAYER_SPEED,
            )
        } else {
            Vec3::ZERO
        };
        self.player_pos = Vec3::new(
            self.player_pos.x + self.player_vel.x * dt32,
            self.player_pos.y + self.player_vel.y * dt32,
            self.player_pos.z + self.player_vel.z * dt32,
        );

        self.time_s += dt;
        self.tick += 1;
    }

    /// Build a snapshot of the current authoritative state.
    pub fn snapshot(&mut self) -> WorldSnapshot {
        let t = self.time_s as f32;
        let mut entities = Vec::with_capacity(NUM_AGENTS as usize + 1);

        // Local player (entity id 0).
        entities.push(EntityState {
            id: 0,
            archetype: 1, // catalog index: "player character"
            flags: FLAG_VISIBLE | FLAG_LOCAL_PLAYER,
            transform: Transform {
                translation: self.player_pos,
                rotation: ue_renderer_protocol::Quat::IDENTITY,
                scale: Vec3::new(1.0, 1.0, 1.0),
            },
            velocity: self.player_vel,
            anim_state: if self.player_vel == Vec3::ZERO { 0 } else { 1 }, // idle/run
            anim_play_rate: 1.0,
        });

        // Orbiting agents (ids 1..=NUM_AGENTS).
        for i in 1..=NUM_AGENTS {
            let phase = i as f32 * std::f32::consts::TAU / NUM_AGENTS as f32;
            let angle = t * 0.5 + phase;
            let radius = 1500.0;
            let pos = Vec3::new(radius * angle.cos(), radius * angle.sin(), 0.0);
            entities.push(EntityState {
                id: i,
                archetype: 2, // catalog index: "agent"
                flags: FLAG_VISIBLE,
                transform: Transform {
                    translation: pos,
                    rotation: ue_renderer_protocol::Quat::IDENTITY,
                    scale: Vec3::new(1.0, 1.0, 1.0),
                },
                velocity: Vec3::ZERO,
                anim_state: 1,
                anim_play_rate: 1.0,
            });
        }

        // On the first snapshot, tell the renderer to create proxies.
        let mut events = Vec::new();
        if !self.spawned {
            self.spawned = true;
            for e in &entities {
                events.push(Event::Spawn {
                    id: e.id,
                    archetype: e.archetype,
                });
            }
        }

        // Streaming directives: one activation source following the player, and a
        // "Gameplay" data layer kept active. This is what the renderer maps onto
        // UWorldPartitionStreamingSourceComponent + SetDataLayerRuntimeState.
        let streaming = StreamingState {
            sources: vec![StreamingSource {
                position: self.player_pos,
                radius: STREAM_RADIUS,
                grid: 0,
                target: StreamingTargetState::Activated,
                priority: 100,
            }],
            data_layers: vec![DataLayerState {
                name: "Gameplay".to_string(),
                state: DataLayerRuntimeState::Activated,
            }],
        };

        WorldSnapshot {
            tick: self.tick,
            sim_time_s: self.time_s,
            entities,
            events,
            streaming,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_snapshots() {
        // Two sims fed identical inputs/timesteps must produce identical bytes.
        let mut a = Sim::new();
        let mut b = Sim::new();
        let input = PlayerInput {
            move_dir: Vec3::new(1.0, 0.0, 0.0),
            ..Default::default()
        };
        for _ in 0..120 {
            a.apply_input(input);
            b.apply_input(input);
            a.step(1.0 / 60.0);
            b.step(1.0 / 60.0);
        }
        assert_eq!(a.snapshot().encode(), b.snapshot().encode());
    }

    #[test]
    fn input_moves_player_and_first_tick_spawns() {
        let mut sim = Sim::new();
        sim.apply_input(PlayerInput {
            move_dir: Vec3::new(1.0, 0.0, 0.0),
            ..Default::default()
        });
        sim.step(1.0);
        let snap = sim.snapshot();
        // First snapshot emits Spawn events for every entity.
        assert_eq!(snap.events.len(), snap.entities.len());
        // Player (id 0) moved along +X at PLAYER_SPEED for 1 s.
        let player = snap.entities.iter().find(|e| e.id == 0).unwrap();
        assert!((player.transform.translation.x - PLAYER_SPEED).abs() < 1e-3);
        assert_ne!(player.anim_state, 0, "moving player should not be idle");
        // Second snapshot no longer re-spawns.
        sim.step(1.0);
        assert!(sim.snapshot().events.is_empty());
    }
}
