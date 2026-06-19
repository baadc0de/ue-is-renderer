//! # ue-renderer-protocol
//!
//! The contract between the **authoritative Rust core** and the **Unreal Engine
//! renderer adapter** (the thin C++ plugin described in `bridge/README.md`).
//!
//! Two messages cross the boundary:
//!
//! * [`WorldSnapshot`] — Rust → UE, once per authoritative tick. The renderer
//!   maps it onto proxy actors / Mass entities / direct-`FScene` primitives and
//!   interpolates between successive snapshots for display (see [`WorldSnapshot::interpolate`]).
//! * [`PlayerInput`] — UE → Rust, the local player's intent.
//!
//! ## Why a hand-rolled wire format?
//!
//! The format is deliberately explicit and dependency-free so the UE C++ side
//! can mirror the decoder byte-for-byte, and so the ABI is stable and auditable.
//! All integers/floats are **little-endian**. Sequences are a `u32` count
//! followed by that many elements. Strings are a `u32` byte-length followed by
//! UTF-8 bytes.
//!
//! A [`WorldSnapshot`] on the wire is:
//!
//! ```text
//! magic   : [u8; 4]  = b"UERS"
//! version : u16      = WIRE_VERSION
//! flags   : u16      = 0 (reserved)
//! body    : <WorldSnapshot fields, in declaration order>
//! ```
//!
//! A [`PlayerInput`] is identical but with magic `b"UEPI"`.
//!
//! This crate forbids `unsafe`; the only `unsafe` in the project lives in the
//! [`ue-bridge-ffi`](../ue_bridge_ffi/index.html) crate's C-ABI shims.

#![forbid(unsafe_code)]

use std::collections::HashMap;

/// Magic prefix for a framed [`WorldSnapshot`].
pub const WIRE_MAGIC_SNAPSHOT: [u8; 4] = *b"UERS";
/// Magic prefix for a framed [`PlayerInput`].
pub const WIRE_MAGIC_INPUT: [u8; 4] = *b"UEPI";
/// Wire format version. Bump on any breaking layout change.
pub const WIRE_VERSION: u16 = 1;

/// Upper bound on a decoded sequence length, to bound work on malformed input.
const MAX_SEQ_LEN: usize = 8_000_000;

// ---------------------------------------------------------------------------
// Math
// ---------------------------------------------------------------------------

/// Linear interpolation between two scalars.
#[inline]
pub fn lerp_f32(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// A 3D vector (UE world units / cm).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Vec3 {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

impl Vec3 {
    pub const ZERO: Vec3 = Vec3 {
        x: 0.0,
        y: 0.0,
        z: 0.0,
    };

    #[inline]
    pub const fn new(x: f32, y: f32, z: f32) -> Self {
        Self { x, y, z }
    }

    /// Component-wise linear interpolation.
    #[inline]
    pub fn lerp(self, o: Vec3, t: f32) -> Vec3 {
        Vec3::new(
            lerp_f32(self.x, o.x, t),
            lerp_f32(self.y, o.y, t),
            lerp_f32(self.z, o.z, t),
        )
    }
}

/// A quaternion rotation `(x, y, z, w)`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Quat {
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub w: f32,
}

impl Quat {
    pub const IDENTITY: Quat = Quat {
        x: 0.0,
        y: 0.0,
        z: 0.0,
        w: 1.0,
    };

    #[inline]
    pub const fn new(x: f32, y: f32, z: f32, w: f32) -> Self {
        Self { x, y, z, w }
    }

    #[inline]
    pub fn dot(self, o: Quat) -> f32 {
        self.x * o.x + self.y * o.y + self.z * o.z + self.w * o.w
    }

    #[inline]
    pub fn length(self) -> f32 {
        self.dot(self).sqrt()
    }

    /// Returns a unit-length copy (or identity if degenerate).
    #[inline]
    pub fn normalized(self) -> Quat {
        let len = self.length();
        if len > 0.0 {
            Quat::new(self.x / len, self.y / len, self.z / len, self.w / len)
        } else {
            Quat::IDENTITY
        }
    }

    /// Normalized lerp along the shortest arc. This is the cheap, robust choice
    /// for client-side interpolation between authoritative ticks; slerp is
    /// rarely worth it at the small angular deltas seen between adjacent ticks.
    pub fn nlerp(self, mut o: Quat, t: f32) -> Quat {
        if self.dot(o) < 0.0 {
            o = Quat::new(-o.x, -o.y, -o.z, -o.w);
        }
        Quat::new(
            lerp_f32(self.x, o.x, t),
            lerp_f32(self.y, o.y, t),
            lerp_f32(self.z, o.z, t),
            lerp_f32(self.w, o.w, t),
        )
        .normalized()
    }
}

/// A full transform (translation / rotation / scale), mirroring UE's `FTransform`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Transform {
    pub translation: Vec3,
    pub rotation: Quat,
    pub scale: Vec3,
}

impl Transform {
    pub const IDENTITY: Transform = Transform {
        translation: Vec3::ZERO,
        rotation: Quat::IDENTITY,
        scale: Vec3::new(1.0, 1.0, 1.0),
    };

    /// Interpolate two transforms (lerp translation/scale, nlerp rotation).
    pub fn interpolate(self, o: Transform, t: f32) -> Transform {
        Transform {
            translation: self.translation.lerp(o.translation, t),
            rotation: self.rotation.nlerp(o.rotation, t),
            scale: self.scale.lerp(o.scale, t),
        }
    }
}

// ---------------------------------------------------------------------------
// Entities & events
// ---------------------------------------------------------------------------

/// `flags` bit: the entity should be rendered.
pub const FLAG_VISIBLE: u32 = 1 << 0;
/// `flags` bit: this entity is the local player (UE may extrapolate/predict it).
pub const FLAG_LOCAL_PLAYER: u32 = 1 << 1;

/// One renderable entity, owned authoritatively by Rust and "puppeted" by UE.
#[derive(Debug, Clone, PartialEq)]
pub struct EntityState {
    /// Stable id used to match entities across snapshots (for interpolation).
    pub id: u64,
    /// Index into the cooked UE content catalog (which mesh/skeleton to use).
    /// The catalog is authored in-editor and baked at cook time — see
    /// `docs/evaluation.md` §2.B.
    pub archetype: u32,
    /// Bitfield of `FLAG_*`.
    pub flags: u32,
    pub transform: Transform,
    /// Linear velocity (cm/s), used by the renderer for extrapolation.
    pub velocity: Vec3,
    /// Opaque locomotion/anim state id handed to the UE AnimBP (see §2.C/§3).
    pub anim_state: u16,
    pub anim_play_rate: f32,
}

/// Transient, fire-once events emitted by the authority for a single tick.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// A new entity appeared this tick (renderer should create a proxy).
    Spawn { id: u64, archetype: u32 },
    /// An entity was removed this tick (renderer should destroy its proxy).
    Despawn { id: u64 },
    /// A one-shot cosmetic cue (VFX/audio) at a world location.
    OneShot { cue: u32, position: Vec3 },
}

// ---------------------------------------------------------------------------
// Streaming directives (Rust decides; UE executes — see §2.B)
// ---------------------------------------------------------------------------

/// Desired World Partition cell state, mirroring `EStreamingSourceTargetState`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamingTargetState {
    /// In memory but not visible.
    Loaded,
    /// In memory and visible.
    Activated,
}

/// Desired Data Layer runtime state, mirroring `EDataLayerRuntimeState`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataLayerRuntimeState {
    Unloaded,
    Loaded,
    Activated,
}

/// A synthetic World Partition streaming source the renderer registers on UE's
/// `UWorldPartitionSubsystem` (via `UWorldPartitionStreamingSourceComponent` or
/// a custom `IWorldPartitionStreamingSourceProvider`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StreamingSource {
    pub position: Vec3,
    pub radius: f32,
    /// Target grid id (0 = all/default).
    pub grid: u32,
    pub target: StreamingTargetState,
    /// Higher priority preempts unload of lower-priority sources.
    pub priority: u8,
}

/// A Data Layer the renderer should drive via `SetDataLayerRuntimeState`.
#[derive(Debug, Clone, PartialEq)]
pub struct DataLayerState {
    /// The cooked Data Layer asset name.
    pub name: String,
    pub state: DataLayerRuntimeState,
}

/// The full set of streaming directives for a tick. The renderer makes UE's
/// streaming match this, then gates visibility on `IsStreamingCompleted`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StreamingState {
    pub sources: Vec<StreamingSource>,
    pub data_layers: Vec<DataLayerState>,
}

// ---------------------------------------------------------------------------
// Snapshot & input (the two framed messages)
// ---------------------------------------------------------------------------

/// A complete authoritative view of the world for one tick (Rust → UE).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct WorldSnapshot {
    /// Monotonic authoritative tick number.
    pub tick: u64,
    /// Authoritative simulation time in seconds.
    pub sim_time_s: f64,
    pub entities: Vec<EntityState>,
    pub events: Vec<Event>,
    pub streaming: StreamingState,
}

impl WorldSnapshot {
    /// Encode to a framed byte buffer (`magic | version | flags | body`).
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(64 + self.entities.len() * 48);
        out.extend_from_slice(&WIRE_MAGIC_SNAPSHOT);
        WIRE_VERSION.write(&mut out);
        0u16.write(&mut out); // flags (reserved)
        Codec::write(self, &mut out);
        out
    }

    /// Decode from a framed byte buffer produced by [`WorldSnapshot::encode`].
    pub fn decode(buf: &[u8]) -> Result<Self, DecodeError> {
        let mut r = Reader::new(buf);
        check_frame(&mut r, &WIRE_MAGIC_SNAPSHOT)?;
        <Self as Codec>::read(&mut r)
    }

    /// Client-side interpolation between two authoritative snapshots, where `a`
    /// is older and `b` is newer, with `t` in `[0, 1]`.
    ///
    /// Entities are matched by [`EntityState::id`]: present in both → interpolated;
    /// only in `b` → taken as-is (it just spawned); only in `a` → dropped (it
    /// despawned). Discrete fields (archetype/flags/anim) and the event/streaming
    /// lists are taken from the newer snapshot `b`.
    ///
    /// This is the primitive the renderer uses to decouple its (high) frame rate
    /// from the (lower) authoritative tick rate — see `docs/evaluation.md` §2.C.
    pub fn interpolate(a: &WorldSnapshot, b: &WorldSnapshot, t: f32) -> WorldSnapshot {
        let t = t.clamp(0.0, 1.0);
        let mut older: HashMap<u64, &EntityState> = HashMap::with_capacity(a.entities.len());
        for e in &a.entities {
            older.insert(e.id, e);
        }
        let entities = b
            .entities
            .iter()
            .map(|be| match older.get(&be.id) {
                Some(ae) => EntityState {
                    id: be.id,
                    archetype: be.archetype,
                    flags: be.flags,
                    transform: ae.transform.interpolate(be.transform, t),
                    velocity: ae.velocity.lerp(be.velocity, t),
                    anim_state: be.anim_state,
                    anim_play_rate: be.anim_play_rate,
                },
                None => be.clone(),
            })
            .collect();
        WorldSnapshot {
            tick: b.tick,
            sim_time_s: a.sim_time_s + (b.sim_time_s - a.sim_time_s) * t as f64,
            entities,
            events: b.events.clone(),
            streaming: b.streaming.clone(),
        }
    }
}

/// The local player's intent for a tick (UE → Rust).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct PlayerInput {
    /// Desired move direction in world space (need not be normalized).
    pub move_dir: Vec3,
    /// Look direction / aim (radians: yaw, pitch).
    pub yaw: f32,
    pub pitch: f32,
    /// Bitfield of pressed buttons (game-defined).
    pub buttons: u32,
}

impl PlayerInput {
    /// Encode to a framed byte buffer (`magic | version | flags | body`).
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(32);
        out.extend_from_slice(&WIRE_MAGIC_INPUT);
        WIRE_VERSION.write(&mut out);
        0u16.write(&mut out);
        Codec::write(self, &mut out);
        out
    }

    /// Decode from a framed byte buffer produced by [`PlayerInput::encode`].
    pub fn decode(buf: &[u8]) -> Result<Self, DecodeError> {
        let mut r = Reader::new(buf);
        check_frame(&mut r, &WIRE_MAGIC_INPUT)?;
        <Self as Codec>::read(&mut r)
    }
}

// ---------------------------------------------------------------------------
// Codec plumbing
// ---------------------------------------------------------------------------

/// Error returned when decoding a malformed buffer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeError {
    /// The 4-byte magic prefix did not match the expected message kind.
    BadMagic,
    /// The wire version is not supported by this build.
    UnsupportedVersion(u16),
    /// The buffer ended before a value could be fully read.
    UnexpectedEof,
    /// An enum tag byte was out of range.
    BadTag(u8),
    /// A string field was not valid UTF-8.
    BadUtf8,
    /// A declared sequence length exceeded [`MAX_SEQ_LEN`].
    SequenceTooLong(usize),
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DecodeError::BadMagic => write!(f, "bad magic prefix"),
            DecodeError::UnsupportedVersion(v) => write!(f, "unsupported wire version {v}"),
            DecodeError::UnexpectedEof => write!(f, "unexpected end of buffer"),
            DecodeError::BadTag(t) => write!(f, "invalid enum tag {t}"),
            DecodeError::BadUtf8 => write!(f, "invalid utf-8 in string"),
            DecodeError::SequenceTooLong(n) => write!(f, "sequence length {n} exceeds limit"),
        }
    }
}

impl std::error::Error for DecodeError {}

/// A bounds-checked cursor over a byte slice.
pub struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    #[inline]
    pub fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    #[inline]
    fn take(&mut self, n: usize) -> Result<&'a [u8], DecodeError> {
        let end = self.pos.checked_add(n).ok_or(DecodeError::UnexpectedEof)?;
        if end > self.buf.len() {
            return Err(DecodeError::UnexpectedEof);
        }
        let slice = &self.buf[self.pos..end];
        self.pos = end;
        Ok(slice)
    }

    /// Bytes not yet consumed.
    #[inline]
    pub fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }
}

/// Validate a framed message's `magic | version | flags` header.
fn check_frame(r: &mut Reader, expected_magic: &[u8; 4]) -> Result<(), DecodeError> {
    let magic = r.take(4)?;
    if magic != expected_magic {
        return Err(DecodeError::BadMagic);
    }
    let version = u16::read(r)?;
    if version != WIRE_VERSION {
        return Err(DecodeError::UnsupportedVersion(version));
    }
    let _flags = u16::read(r)?;
    Ok(())
}

/// Symmetric little-endian serializer/deserializer for the wire types.
pub trait Codec: Sized {
    fn write(&self, out: &mut Vec<u8>);
    fn read(r: &mut Reader) -> Result<Self, DecodeError>;
}

macro_rules! impl_codec_num {
    ($($t:ty),* $(,)?) => {$(
        impl Codec for $t {
            #[inline]
            fn write(&self, out: &mut Vec<u8>) {
                out.extend_from_slice(&self.to_le_bytes());
            }
            #[inline]
            fn read(r: &mut Reader) -> Result<Self, DecodeError> {
                const N: usize = core::mem::size_of::<$t>();
                let bytes = r.take(N)?;
                let mut arr = [0u8; N];
                arr.copy_from_slice(bytes);
                Ok(<$t>::from_le_bytes(arr))
            }
        }
    )*};
}

impl_codec_num!(u8, u16, u32, u64, i32, i64, f32, f64);

impl<T: Codec> Codec for Vec<T> {
    fn write(&self, out: &mut Vec<u8>) {
        (self.len() as u32).write(out);
        for item in self {
            item.write(out);
        }
    }

    fn read(r: &mut Reader) -> Result<Self, DecodeError> {
        let count = u32::read(r)? as usize;
        if count > MAX_SEQ_LEN {
            return Err(DecodeError::SequenceTooLong(count));
        }
        // Do not pre-reserve `count` (it is attacker-controlled); cap the hint.
        let mut v = Vec::with_capacity(count.min(1024));
        for _ in 0..count {
            v.push(T::read(r)?);
        }
        Ok(v)
    }
}

impl Codec for String {
    fn write(&self, out: &mut Vec<u8>) {
        (self.len() as u32).write(out);
        out.extend_from_slice(self.as_bytes());
    }

    fn read(r: &mut Reader) -> Result<Self, DecodeError> {
        let len = u32::read(r)? as usize;
        if len > MAX_SEQ_LEN {
            return Err(DecodeError::SequenceTooLong(len));
        }
        let bytes = r.take(len)?;
        core::str::from_utf8(bytes)
            .map(|s| s.to_owned())
            .map_err(|_| DecodeError::BadUtf8)
    }
}

impl Codec for Vec3 {
    fn write(&self, out: &mut Vec<u8>) {
        self.x.write(out);
        self.y.write(out);
        self.z.write(out);
    }
    fn read(r: &mut Reader) -> Result<Self, DecodeError> {
        Ok(Vec3::new(f32::read(r)?, f32::read(r)?, f32::read(r)?))
    }
}

impl Codec for Quat {
    fn write(&self, out: &mut Vec<u8>) {
        self.x.write(out);
        self.y.write(out);
        self.z.write(out);
        self.w.write(out);
    }
    fn read(r: &mut Reader) -> Result<Self, DecodeError> {
        Ok(Quat::new(
            f32::read(r)?,
            f32::read(r)?,
            f32::read(r)?,
            f32::read(r)?,
        ))
    }
}

impl Codec for Transform {
    fn write(&self, out: &mut Vec<u8>) {
        self.translation.write(out);
        self.rotation.write(out);
        self.scale.write(out);
    }
    fn read(r: &mut Reader) -> Result<Self, DecodeError> {
        Ok(Transform {
            translation: Vec3::read(r)?,
            rotation: Quat::read(r)?,
            scale: Vec3::read(r)?,
        })
    }
}

impl Codec for EntityState {
    fn write(&self, out: &mut Vec<u8>) {
        self.id.write(out);
        self.archetype.write(out);
        self.flags.write(out);
        self.transform.write(out);
        self.velocity.write(out);
        self.anim_state.write(out);
        self.anim_play_rate.write(out);
    }
    fn read(r: &mut Reader) -> Result<Self, DecodeError> {
        Ok(EntityState {
            id: u64::read(r)?,
            archetype: u32::read(r)?,
            flags: u32::read(r)?,
            transform: Transform::read(r)?,
            velocity: Vec3::read(r)?,
            anim_state: u16::read(r)?,
            anim_play_rate: f32::read(r)?,
        })
    }
}

impl Codec for Event {
    fn write(&self, out: &mut Vec<u8>) {
        match self {
            Event::Spawn { id, archetype } => {
                0u8.write(out);
                id.write(out);
                archetype.write(out);
            }
            Event::Despawn { id } => {
                1u8.write(out);
                id.write(out);
            }
            Event::OneShot { cue, position } => {
                2u8.write(out);
                cue.write(out);
                position.write(out);
            }
        }
    }
    fn read(r: &mut Reader) -> Result<Self, DecodeError> {
        match u8::read(r)? {
            0 => Ok(Event::Spawn {
                id: u64::read(r)?,
                archetype: u32::read(r)?,
            }),
            1 => Ok(Event::Despawn { id: u64::read(r)? }),
            2 => Ok(Event::OneShot {
                cue: u32::read(r)?,
                position: Vec3::read(r)?,
            }),
            t => Err(DecodeError::BadTag(t)),
        }
    }
}

impl Codec for StreamingTargetState {
    fn write(&self, out: &mut Vec<u8>) {
        let tag: u8 = match self {
            StreamingTargetState::Loaded => 0,
            StreamingTargetState::Activated => 1,
        };
        tag.write(out);
    }
    fn read(r: &mut Reader) -> Result<Self, DecodeError> {
        match u8::read(r)? {
            0 => Ok(StreamingTargetState::Loaded),
            1 => Ok(StreamingTargetState::Activated),
            t => Err(DecodeError::BadTag(t)),
        }
    }
}

impl Codec for DataLayerRuntimeState {
    fn write(&self, out: &mut Vec<u8>) {
        let tag: u8 = match self {
            DataLayerRuntimeState::Unloaded => 0,
            DataLayerRuntimeState::Loaded => 1,
            DataLayerRuntimeState::Activated => 2,
        };
        tag.write(out);
    }
    fn read(r: &mut Reader) -> Result<Self, DecodeError> {
        match u8::read(r)? {
            0 => Ok(DataLayerRuntimeState::Unloaded),
            1 => Ok(DataLayerRuntimeState::Loaded),
            2 => Ok(DataLayerRuntimeState::Activated),
            t => Err(DecodeError::BadTag(t)),
        }
    }
}

impl Codec for StreamingSource {
    fn write(&self, out: &mut Vec<u8>) {
        self.position.write(out);
        self.radius.write(out);
        self.grid.write(out);
        self.target.write(out);
        self.priority.write(out);
    }
    fn read(r: &mut Reader) -> Result<Self, DecodeError> {
        Ok(StreamingSource {
            position: Vec3::read(r)?,
            radius: f32::read(r)?,
            grid: u32::read(r)?,
            target: StreamingTargetState::read(r)?,
            priority: u8::read(r)?,
        })
    }
}

impl Codec for DataLayerState {
    fn write(&self, out: &mut Vec<u8>) {
        self.name.write(out);
        self.state.write(out);
    }
    fn read(r: &mut Reader) -> Result<Self, DecodeError> {
        Ok(DataLayerState {
            name: String::read(r)?,
            state: DataLayerRuntimeState::read(r)?,
        })
    }
}

impl Codec for StreamingState {
    fn write(&self, out: &mut Vec<u8>) {
        self.sources.write(out);
        self.data_layers.write(out);
    }
    fn read(r: &mut Reader) -> Result<Self, DecodeError> {
        Ok(StreamingState {
            sources: Vec::<StreamingSource>::read(r)?,
            data_layers: Vec::<DataLayerState>::read(r)?,
        })
    }
}

impl Codec for WorldSnapshot {
    fn write(&self, out: &mut Vec<u8>) {
        self.tick.write(out);
        self.sim_time_s.write(out);
        self.entities.write(out);
        self.events.write(out);
        self.streaming.write(out);
    }
    fn read(r: &mut Reader) -> Result<Self, DecodeError> {
        Ok(WorldSnapshot {
            tick: u64::read(r)?,
            sim_time_s: f64::read(r)?,
            entities: Vec::<EntityState>::read(r)?,
            events: Vec::<Event>::read(r)?,
            streaming: StreamingState::read(r)?,
        })
    }
}

impl Codec for PlayerInput {
    fn write(&self, out: &mut Vec<u8>) {
        self.move_dir.write(out);
        self.yaw.write(out);
        self.pitch.write(out);
        self.buttons.write(out);
    }
    fn read(r: &mut Reader) -> Result<Self, DecodeError> {
        Ok(PlayerInput {
            move_dir: Vec3::read(r)?,
            yaw: f32::read(r)?,
            pitch: f32::read(r)?,
            buttons: u32::read(r)?,
        })
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_snapshot() -> WorldSnapshot {
        WorldSnapshot {
            tick: 42,
            sim_time_s: 1.5,
            entities: vec![
                EntityState {
                    id: 1,
                    archetype: 7,
                    flags: FLAG_VISIBLE | FLAG_LOCAL_PLAYER,
                    transform: Transform {
                        translation: Vec3::new(10.0, -20.0, 30.0),
                        rotation: Quat::new(0.0, 0.0, 0.70710677, 0.70710677),
                        scale: Vec3::new(1.0, 1.0, 1.0),
                    },
                    velocity: Vec3::new(1.0, 0.0, 0.0),
                    anim_state: 3,
                    anim_play_rate: 1.25,
                },
                EntityState {
                    id: 2,
                    archetype: 0,
                    flags: FLAG_VISIBLE,
                    transform: Transform::IDENTITY,
                    velocity: Vec3::ZERO,
                    anim_state: 0,
                    anim_play_rate: 1.0,
                },
            ],
            events: vec![
                Event::Spawn {
                    id: 2,
                    archetype: 0,
                },
                Event::OneShot {
                    cue: 99,
                    position: Vec3::new(5.0, 5.0, 0.0),
                },
                Event::Despawn { id: 17 },
            ],
            streaming: StreamingState {
                sources: vec![StreamingSource {
                    position: Vec3::new(100.0, 200.0, 0.0),
                    radius: 5000.0,
                    grid: 0,
                    target: StreamingTargetState::Activated,
                    priority: 200,
                }],
                data_layers: vec![DataLayerState {
                    name: "Gameplay".to_string(),
                    state: DataLayerRuntimeState::Activated,
                }],
            },
        }
    }

    #[test]
    fn snapshot_roundtrip() {
        let snap = sample_snapshot();
        let bytes = snap.encode();
        let back = WorldSnapshot::decode(&bytes).expect("decode");
        assert_eq!(snap, back);
    }

    #[test]
    fn input_roundtrip() {
        let input = PlayerInput {
            move_dir: Vec3::new(0.0, 1.0, 0.0),
            yaw: 0.5,
            pitch: -0.25,
            buttons: 0b1010,
        };
        let bytes = input.encode();
        assert_eq!(PlayerInput::decode(&bytes).expect("decode"), input);
    }

    #[test]
    fn rejects_bad_frames() {
        assert_eq!(WorldSnapshot::decode(&[]), Err(DecodeError::UnexpectedEof));
        // Wrong magic (this is an input frame, not a snapshot).
        let input_bytes = PlayerInput::default().encode();
        assert_eq!(
            WorldSnapshot::decode(&input_bytes),
            Err(DecodeError::BadMagic)
        );
        // Truncated body after a valid header.
        let mut truncated = WIRE_MAGIC_SNAPSHOT.to_vec();
        WIRE_VERSION.write(&mut truncated);
        0u16.write(&mut truncated);
        assert_eq!(
            WorldSnapshot::decode(&truncated),
            Err(DecodeError::UnexpectedEof)
        );
    }

    #[test]
    fn rejects_unsupported_version() {
        let mut bytes = WIRE_MAGIC_SNAPSHOT.to_vec();
        (WIRE_VERSION + 1).write(&mut bytes);
        0u16.write(&mut bytes);
        assert_eq!(
            WorldSnapshot::decode(&bytes),
            Err(DecodeError::UnsupportedVersion(WIRE_VERSION + 1))
        );
    }

    #[test]
    fn transform_interpolation_endpoints_and_mid() {
        let a = Transform::IDENTITY;
        let b = Transform {
            translation: Vec3::new(10.0, 0.0, 0.0),
            rotation: Quat::IDENTITY,
            scale: Vec3::new(3.0, 3.0, 3.0),
        };
        assert_eq!(a.interpolate(b, 0.0), a);
        assert_eq!(a.interpolate(b, 1.0).translation, b.translation);
        let mid = a.interpolate(b, 0.5);
        assert_eq!(mid.translation, Vec3::new(5.0, 0.0, 0.0));
        assert_eq!(mid.scale, Vec3::new(2.0, 2.0, 2.0));
    }

    #[test]
    fn nlerp_stays_normalized() {
        let a = Quat::IDENTITY;
        let b = Quat::new(0.0, 0.0, 1.0, 0.0); // 180° about Z
        let q = a.nlerp(b, 0.5);
        assert!((q.length() - 1.0).abs() < 1e-5, "len was {}", q.length());
    }

    #[test]
    fn snapshot_interpolation_matches_by_id() {
        // a: {1, 2}; b: {2 (moved), 3 (spawned)}. Expect {2 interpolated, 3 as-is}.
        let a = WorldSnapshot {
            tick: 1,
            sim_time_s: 0.0,
            entities: vec![
                EntityState {
                    id: 1,
                    archetype: 0,
                    flags: FLAG_VISIBLE,
                    transform: Transform::IDENTITY,
                    velocity: Vec3::ZERO,
                    anim_state: 0,
                    anim_play_rate: 1.0,
                },
                EntityState {
                    id: 2,
                    archetype: 0,
                    flags: FLAG_VISIBLE,
                    transform: Transform::IDENTITY,
                    velocity: Vec3::ZERO,
                    anim_state: 0,
                    anim_play_rate: 1.0,
                },
            ],
            events: vec![],
            streaming: StreamingState::default(),
        };
        let mut moved = Transform::IDENTITY;
        moved.translation = Vec3::new(8.0, 0.0, 0.0);
        let b = WorldSnapshot {
            tick: 2,
            sim_time_s: 1.0,
            entities: vec![
                EntityState {
                    id: 2,
                    archetype: 0,
                    flags: FLAG_VISIBLE,
                    transform: moved,
                    velocity: Vec3::ZERO,
                    anim_state: 0,
                    anim_play_rate: 1.0,
                },
                EntityState {
                    id: 3,
                    archetype: 5,
                    flags: FLAG_VISIBLE,
                    transform: moved,
                    velocity: Vec3::ZERO,
                    anim_state: 0,
                    anim_play_rate: 1.0,
                },
            ],
            events: vec![Event::Spawn {
                id: 3,
                archetype: 5,
            }],
            streaming: StreamingState::default(),
        };

        let mid = WorldSnapshot::interpolate(&a, &b, 0.5);
        assert_eq!(
            mid.entities.len(),
            2,
            "entity 1 (despawned) should be dropped"
        );
        let e2 = mid.entities.iter().find(|e| e.id == 2).unwrap();
        assert_eq!(e2.transform.translation, Vec3::new(4.0, 0.0, 0.0));
        let e3 = mid.entities.iter().find(|e| e.id == 3).unwrap();
        assert_eq!(
            e3.transform.translation,
            Vec3::new(8.0, 0.0, 0.0),
            "spawned entity taken as-is"
        );
        assert_eq!(mid.sim_time_s, 0.5);
        assert_eq!(mid.events, b.events, "events come from the newer snapshot");
    }
}
