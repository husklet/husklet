// Test helper for the engine→compositor allocation-generation channel. Creates a real host IOSurface,
// looks up the compositor GPU Mach bridge, and sends the production protocol message carrying its
// send-right + its id + its allocation generation. The Rust harness starts the real bridge, runs this,
// and asserts hl_display::metal::iosurface_generation(id) reports the generation we sent — proving the
// new generation field flows over the real mach ABI into the compositor's authenticated metadata.
//
// argv[1] = generation to send. Prints "id=<IOSurfaceGetID> gen=<generation>". macOS-only.
#include <CoreFoundation/CoreFoundation.h>
#include <IOSurface/IOSurface.h>
#include <mach/mach.h>
#include <servers/bootstrap.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

// MUST match the engine/compositor Mach bridge ABI.
typedef struct {
    mach_msg_header_t header;
    mach_msg_body_t body;
    mach_msg_port_descriptor_t port;
    uint32_t id;
    uint32_t generation;
} hl_gpu_msg_t;

static void put_i32(CFMutableDictionaryRef d, CFStringRef k, int32_t v) {
    CFNumberRef n = CFNumberCreate(NULL, kCFNumberSInt32Type, &v);
    CFDictionarySetValue(d, k, n);
    CFRelease(n);
}

int main(int argc, char **argv) {
    uint32_t generation = (argc > 1) ? (uint32_t)strtoul(argv[1], NULL, 10) : 1;

    CFMutableDictionaryRef props = CFDictionaryCreateMutable(
        NULL, 0, &kCFTypeDictionaryKeyCallBacks, &kCFTypeDictionaryValueCallBacks);
    if (!props) return 2;
    put_i32(props, kIOSurfaceWidth, 16);
    put_i32(props, kIOSurfaceHeight, 8);
    put_i32(props, kIOSurfaceBytesPerElement, 4);
    put_i32(props, kIOSurfaceBytesPerRow, 64);
    put_i32(props, kIOSurfacePixelFormat, 0x42475241 /* 'BGRA' */);
    put_i32(props, kIOSurfaceIsGlobal, 1);
    IOSurfaceRef surf = IOSurfaceCreate(props);
    CFRelease(props);
    if (!surf) return 3;
    uint32_t id = IOSurfaceGetID(surf);

    const char *bridge = getenv("HL_GPU_BRIDGE_NAME");
    if (!bridge || !*bridge) bridge = "com.hl.display.gpu";
    mach_port_t server = MACH_PORT_NULL;
    if (bootstrap_look_up(bootstrap_port, (char *)bridge, &server) != KERN_SUCCESS) return 4;
    mach_port_t port = IOSurfaceCreateMachPort(surf);
    if (port == MACH_PORT_NULL) return 5;

    hl_gpu_msg_t msg;
    memset(&msg, 0, sizeof msg);
    msg.header.msgh_bits = MACH_MSGH_BITS_COMPLEX | MACH_MSGH_BITS(MACH_MSG_TYPE_COPY_SEND, 0);
    msg.header.msgh_size = sizeof msg;
    msg.header.msgh_remote_port = server;
    msg.header.msgh_local_port = MACH_PORT_NULL;
    msg.header.msgh_id = 1;
    msg.body.msgh_descriptor_count = 1;
    msg.port.name = port;
    msg.port.disposition = MACH_MSG_TYPE_COPY_SEND;
    msg.port.type = MACH_MSG_PORT_DESCRIPTOR;
    msg.id = id;
    msg.generation = generation;
    kern_return_t kr = mach_msg(&msg.header, MACH_SEND_MSG, sizeof msg, 0, MACH_PORT_NULL,
                                MACH_MSG_TIMEOUT_NONE, MACH_PORT_NULL);
    mach_port_deallocate(mach_task_self(), port);
    mach_port_deallocate(mach_task_self(), server);
    if (kr != KERN_SUCCESS) return 6;

    printf("id=%u gen=%u\n", id, generation);
    fflush(stdout);
    // Keep the surface (and this process) alive briefly so the receiver can IOSurfaceLookupFromMachPort
    // the still-live send-right before we exit.
    struct timespec ts = {.tv_sec = 0, .tv_nsec = 300 * 1000 * 1000};
    nanosleep(&ts, NULL);
    return 0;
}
