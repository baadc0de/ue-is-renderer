//! # ue-bridge-ffi
//!
//! The narrow, handle-based **C ABI** that an Unreal Engine C++ plugin links
//! against. It is the concrete realization of the integration boundary analyzed
//! in `docs/evaluation.md` §2.C/§2.E and specified in `bridge/README.md`.
//!
//! Design rules (straight from the evaluation's risk mitigations):
//!
//! * **Plain-Rust state, opaque handle.** UE only ever sees an opaque
//!   `UeBridge*`. No `UObject` is owned by Rust; nothing crosses the boundary
//!   except bytes and primitives. (Evaluation R4: the UObject-GC ↔ Rust-ownership
//!   mismatch is avoided by never sharing UObjects.)
//! * **Panic isolation.** Every entry point wraps its body in `catch_unwind` and
//!   returns an error code; a Rust panic must never unwind into UE. (R4)
//! * **Latest-wins, caller-owned buffer.** The renderer pulls the most recent
//!   encoded snapshot into its own reusable buffer on the game thread; the
//!   bridge never blocks and never allocates on the caller's behalf. (§2.C)
//! * **ABI version handshake** via [`ue_bridge_abi_version`].
//!
//! Threading: all functions here are cheap and synchronous and are expected to
//! be called from UE's **game thread**. A production build that runs the Rust
//! authority on its own thread would replace the body of [`ue_bridge_tick`] with
//! a read of a lock-free latest-snapshot slot; the C ABI stays the same.

// These are C-ABI entry points: they accept raw pointers from the (C++) caller
// and dereference them inside explicit `unsafe` blocks. Their preconditions are
// documented per-function in `# Safety` sections and are the caller's
// responsibility, exactly as for any C API. Marking them `unsafe fn` would only
// obscure the C-facing signature without adding any safety, so we opt out of
// clippy's suggestion here.
#![allow(clippy::not_unsafe_ptr_arg_deref)]

use std::os::raw::c_void;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::ptr;
use std::slice;

use ue_authority::Sim;
use ue_renderer_protocol::PlayerInput;

/// Success.
pub const UE_OK: i32 = 0;
/// A required pointer argument was null.
pub const UE_ERR_NULL: i32 = -1;
/// A Rust panic was caught at the boundary (state may be unchanged).
pub const UE_ERR_PANIC: i32 = -2;
/// The caller buffer was too small; `*out_len` holds the required size.
pub const UE_ERR_BUFFER_TOO_SMALL: i32 = -3;
/// The provided input bytes failed to decode.
pub const UE_ERR_DECODE: i32 = -4;

/// The ABI/contract version. The C++ side must check this at load time and
/// refuse to run on a mismatch. Bump on any breaking change to these functions.
pub const UE_BRIDGE_ABI_VERSION: u32 = 1;

/// Opaque handle. Layout is private to Rust; C only ever holds a pointer.
pub struct UeBridge {
    sim: Sim,
    /// The most recently encoded snapshot (latest-wins).
    latest: Vec<u8>,
}

/// Create a new bridge instance. Returns null on allocation failure or panic.
///
/// # Safety
/// The returned pointer must be released exactly once with [`ue_bridge_destroy`].
#[no_mangle]
pub extern "C" fn ue_bridge_create() -> *mut UeBridge {
    catch_unwind(|| {
        let bridge = Box::new(UeBridge {
            sim: Sim::new(),
            latest: Vec::new(),
        });
        Box::into_raw(bridge)
    })
    .unwrap_or(ptr::null_mut())
}

/// Destroy a bridge created by [`ue_bridge_create`]. Null is ignored.
///
/// # Safety
/// `handle` must have come from [`ue_bridge_create`] and not been destroyed.
#[no_mangle]
pub extern "C" fn ue_bridge_destroy(handle: *mut UeBridge) {
    if handle.is_null() {
        return;
    }
    let _ = catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: non-null and owned per the function contract.
        drop(unsafe { Box::from_raw(handle) });
    }));
}

/// Advance the authoritative simulation by `dt_seconds` and refresh the latest
/// snapshot. Returns [`UE_OK`] or an `UE_ERR_*` code.
///
/// # Safety
/// `handle` must be a live bridge pointer.
#[no_mangle]
pub extern "C" fn ue_bridge_tick(handle: *mut UeBridge, dt_seconds: f64) -> i32 {
    if handle.is_null() {
        return UE_ERR_NULL;
    }
    catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: non-null per the check above; exclusive game-thread access.
        let bridge = unsafe { &mut *handle };
        bridge.sim.step(dt_seconds);
        bridge.latest = bridge.sim.snapshot().encode();
        UE_OK
    }))
    .unwrap_or(UE_ERR_PANIC)
}

/// Copy the latest encoded snapshot into the caller-owned buffer (latest-wins).
///
/// On success returns [`UE_OK`] and writes the byte count to `*out_len`. If
/// `cap` is too small (or `out` is null), returns [`UE_ERR_BUFFER_TOO_SMALL`]
/// and still writes the required size to `*out_len`, so the caller can grow its
/// buffer and retry. The caller should keep the buffer between frames.
///
/// # Safety
/// `handle` must be live; `out` must be valid for `cap` bytes (or null);
/// `out_len` must be a valid `usize` pointer.
#[no_mangle]
pub extern "C" fn ue_bridge_latest_snapshot(
    handle: *mut UeBridge,
    out: *mut u8,
    cap: usize,
    out_len: *mut usize,
) -> i32 {
    if handle.is_null() || out_len.is_null() {
        return UE_ERR_NULL;
    }
    catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: non-null per the checks above.
        let bridge = unsafe { &*handle };
        let needed = bridge.latest.len();
        // SAFETY: out_len checked non-null.
        unsafe { *out_len = needed };
        if out.is_null() || needed > cap {
            return UE_ERR_BUFFER_TOO_SMALL;
        }
        // SAFETY: out is valid for `cap >= needed` bytes; regions don't overlap.
        unsafe { ptr::copy_nonoverlapping(bridge.latest.as_ptr(), out, needed) };
        UE_OK
    }))
    .unwrap_or(UE_ERR_PANIC)
}

/// Feed one framed [`PlayerInput`] (UE → Rust). Returns [`UE_OK`],
/// [`UE_ERR_DECODE`] on malformed bytes, or another `UE_ERR_*` code.
///
/// # Safety
/// `handle` must be live; `data` must be valid for `len` bytes.
#[no_mangle]
pub extern "C" fn ue_bridge_push_input(handle: *mut UeBridge, data: *const u8, len: usize) -> i32 {
    if handle.is_null() || data.is_null() {
        return UE_ERR_NULL;
    }
    catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: non-null per the checks above.
        let bridge = unsafe { &mut *handle };
        // SAFETY: data valid for len bytes per the function contract.
        let bytes = unsafe { slice::from_raw_parts(data, len) };
        match PlayerInput::decode(bytes) {
            Ok(input) => {
                bridge.sim.apply_input(input);
                UE_OK
            }
            Err(_) => UE_ERR_DECODE,
        }
    }))
    .unwrap_or(UE_ERR_PANIC)
}

/// Returns the ABI version this library was built with. The C++ side must
/// compare it against its compiled-in expectation before using any other call.
#[no_mangle]
pub extern "C" fn ue_bridge_abi_version() -> u32 {
    UE_BRIDGE_ABI_VERSION
}

/// Reserved no-op to keep a stable symbol for future use; also documents that
/// the handle is an opaque `void*` from C's perspective.
#[no_mangle]
pub extern "C" fn ue_bridge_handle_is_null(handle: *const c_void) -> i32 {
    handle.is_null() as i32
}

#[cfg(test)]
mod tests {
    use super::*;
    use ue_renderer_protocol::{PlayerInput, Vec3, WorldSnapshot};

    #[test]
    fn lifecycle_tick_and_pull() {
        let h = ue_bridge_create();
        assert!(!h.is_null());
        assert_eq!(ue_bridge_abi_version(), UE_BRIDGE_ABI_VERSION);

        // Push input, tick, pull the latest snapshot through the C-style API.
        let input = PlayerInput {
            move_dir: Vec3::new(1.0, 0.0, 0.0),
            ..Default::default()
        };
        let in_bytes = input.encode();
        assert_eq!(
            ue_bridge_push_input(h, in_bytes.as_ptr(), in_bytes.len()),
            UE_OK
        );
        assert_eq!(ue_bridge_tick(h, 1.0 / 60.0), UE_OK);

        // First query with a zero-cap buffer reports the required size.
        let mut needed: usize = 0;
        let rc = ue_bridge_latest_snapshot(h, ptr::null_mut(), 0, &mut needed as *mut usize);
        assert_eq!(rc, UE_ERR_BUFFER_TOO_SMALL);
        assert!(needed > 0);

        // Allocate and pull for real.
        let mut buf = vec![0u8; needed];
        let mut got: usize = 0;
        let rc = ue_bridge_latest_snapshot(h, buf.as_mut_ptr(), buf.len(), &mut got as *mut usize);
        assert_eq!(rc, UE_OK);
        assert_eq!(got, needed);

        // The bytes decode into a real snapshot whose player has moved.
        let snap = WorldSnapshot::decode(&buf[..got]).expect("decode");
        let player = snap.entities.iter().find(|e| e.id == 0).expect("player");
        assert!(player.transform.translation.x > 0.0);

        ue_bridge_destroy(h);
    }

    #[test]
    fn null_and_bad_input_are_rejected_not_crashed() {
        assert_eq!(ue_bridge_tick(ptr::null_mut(), 0.016), UE_ERR_NULL);
        let mut len = 0usize;
        assert_eq!(
            ue_bridge_latest_snapshot(ptr::null_mut(), ptr::null_mut(), 0, &mut len),
            UE_ERR_NULL
        );

        let h = ue_bridge_create();
        let garbage = [0xFFu8; 8];
        assert_eq!(
            ue_bridge_push_input(h, garbage.as_ptr(), garbage.len()),
            UE_ERR_DECODE
        );
        ue_bridge_destroy(h);

        // Destroying null is a safe no-op.
        ue_bridge_destroy(ptr::null_mut());
    }
}
