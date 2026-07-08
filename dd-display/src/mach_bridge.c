// dd-display mach receiver for the GPU rung 2 IOSurface handle bridge.
//
// The engine (dd-jit-darwin) creates a host IOSurface for a guest's dmabuf and sends its send-right +
// global id to this service over Mach IPC (global-id IOSurfaceLookup is restricted on modern macOS, so
// the mach port is the only cross-process handle). We register a bootstrap service, receive (port, id)
// messages, resolve the IOSurface via IOSurfaceLookupFromMachPort, and hand it back to Rust to cache.
//
// Written in C so the Mach message ABI comes straight from <mach/mach.h> (getting it wrong in hand-rolled
// Rust structs is the classic footgun). macOS-only; compiled by dd-display/build.rs and linked with the
// System + IOSurface frameworks.

#include <mach/mach.h>
#include <servers/bootstrap.h>
#include <IOSurface/IOSurface.h>
#include <stdint.h>
#include <string.h>

// The wire message: a complex message carrying one port descriptor + the surface id.
typedef struct {
    mach_msg_header_t header;
    mach_msg_body_t body;
    mach_msg_port_descriptor_t port;
    uint32_t id;
} dd_gpu_msg_t;

static mach_port_t g_recv = MACH_PORT_NULL;

// Register the bootstrap service. Returns KERN_SUCCESS(0) on success, else the kern_return_t.
// Tries check_in (launchd-declared) first, then the dynamic register fallback.
int dd_mach_server_start(const char *name) {
    kern_return_t kr = bootstrap_check_in(bootstrap_port, name, &g_recv);
    if (kr == KERN_SUCCESS) return 0;
    // Dynamic fallback: allocate a receive right + a send right and register it.
    if (mach_port_allocate(mach_task_self(), MACH_PORT_RIGHT_RECEIVE, &g_recv) != KERN_SUCCESS) return kr;
    mach_port_insert_right(mach_task_self(), g_recv, g_recv, MACH_MSG_TYPE_MAKE_SEND);
    kern_return_t kr2 = bootstrap_register(bootstrap_port, (char *)name, g_recv);
    return kr2 == KERN_SUCCESS ? 0 : kr2;
}

// Block until one (id, IOSurface) message arrives. On success returns 0, sets *out_id and returns the
// IOSurfaceRef as an opaque pointer (caller owns it, CFRelease when done). Non-zero = a mach/lookup error.
int dd_mach_recv(uint32_t *out_id, void **out_surface) {
    struct {
        dd_gpu_msg_t msg;
        mach_msg_trailer_t trailer;
    } r;
    memset(&r, 0, sizeof r);
    kern_return_t kr =
        mach_msg(&r.msg.header, MACH_RCV_MSG, 0, sizeof r, g_recv, MACH_MSG_TIMEOUT_NONE, MACH_PORT_NULL);
    if (kr != KERN_SUCCESS) return (int)kr;
    *out_id = r.msg.id;
    IOSurfaceRef s = IOSurfaceLookupFromMachPort(r.msg.port.name);
    mach_port_deallocate(mach_task_self(), r.msg.port.name); // done with the transferred send-right
    if (!s) return -1;
    *out_surface = (void *)s;
    return 0;
}
