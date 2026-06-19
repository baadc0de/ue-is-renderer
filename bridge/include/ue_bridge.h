/*
 * ue_bridge.h — C ABI exported by the `ue-bridge-ffi` crate.
 *
 * This header is the contract an Unreal Engine C++ plugin compiles against. It
 * is hand-written and kept in sync with crates/ue-bridge-ffi/src/lib.rs; it can
 * also be regenerated with cbindgen (see crates/ue-bridge-ffi/cbindgen.toml).
 *
 * All functions are safe to call from UE's game thread. Pointers passed in are
 * borrowed for the duration of the call only. The bridge never blocks and never
 * takes ownership of caller memory. A caught Rust panic is reported as
 * UE_ERR_PANIC instead of unwinding into the host process.
 *
 * See bridge/README.md for how the C++ adapter uses these calls each frame.
 */
#ifndef UE_BRIDGE_H
#define UE_BRIDGE_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Return codes. */
#define UE_OK 0
#define UE_ERR_NULL (-1)             /* a required pointer argument was null */
#define UE_ERR_PANIC (-2)            /* a Rust panic was caught at the boundary */
#define UE_ERR_BUFFER_TOO_SMALL (-3) /* grow to *out_len and retry */
#define UE_ERR_DECODE (-4)           /* input bytes failed to decode */

/* Bump on any breaking change to the functions below. The C++ side must compare
 * this against ue_bridge_abi_version() at load time and refuse a mismatch. */
#define UE_BRIDGE_ABI_VERSION 1u

/* Opaque handle to a bridge instance. */
typedef struct UeBridge UeBridge;

/* Create a bridge instance. Returns NULL on failure. Release with
 * ue_bridge_destroy exactly once. */
UeBridge *ue_bridge_create(void);

/* Destroy a bridge instance. NULL is ignored. */
void ue_bridge_destroy(UeBridge *handle);

/* Advance the authoritative simulation by dt_seconds and refresh the latest
 * snapshot. Returns UE_OK or a UE_ERR_* code. */
int32_t ue_bridge_tick(UeBridge *handle, double dt_seconds);

/* Copy the latest encoded snapshot into the caller-owned buffer (latest-wins).
 *
 * On success: returns UE_OK and writes the byte count to *out_len.
 * If `cap` is too small or `out` is NULL: returns UE_ERR_BUFFER_TOO_SMALL and
 * still writes the required size to *out_len. Keep `out` allocated across frames
 * and only grow it when asked. Decode the bytes with the format documented in
 * crates/ue-renderer-protocol (mirror it in C++). */
int32_t ue_bridge_latest_snapshot(UeBridge *handle, uint8_t *out, size_t cap,
                                   size_t *out_len);

/* Feed one framed PlayerInput (UE -> Rust). Returns UE_OK, UE_ERR_DECODE on bad
 * bytes, or another UE_ERR_* code. */
int32_t ue_bridge_push_input(UeBridge *handle, const uint8_t *data, size_t len);

/* The ABI version this library was built with. */
uint32_t ue_bridge_abi_version(void);

/* Convenience: 1 if the (void*) handle is NULL, else 0. */
int32_t ue_bridge_handle_is_null(const void *handle);

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* UE_BRIDGE_H */
