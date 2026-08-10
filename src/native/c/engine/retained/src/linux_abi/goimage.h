#ifndef HL_LINUX_ABI_GOIMAGE_H
#define HL_LINUX_ABI_GOIMAGE_H

#include <stddef.h>
#include <stdint.h>
#include <string.h>

/*
 * Is `f` (an ELF image of `sz` bytes) a Go main image?  Every Go binary carries the linker's build-info
 * blob, whose magic uniquely identifies one.  A Go runtime OWNS SIGURG for async preemption, which this
 * engine cannot yet honour (see the g_go_image rationale in signal.c), so both loaders latch this and
 * signal delivery suppresses SIGURG for the image.  Shared by the aarch64 and x86-64 loaders: the defect
 * is guest-runtime-shaped, not ISA-shaped, and an x86-64 Go guest whose SIGURG is delivered livelocks
 * (sysmon re-sends the preempt signal forever while the GC waits for a stop-the-world that never lands).
 */
static inline int elf_is_go_image(const uint8_t *f, size_t sz) {
    static const char magic[14] = {(char)0xff, ' ', 'G', 'o', ' ', 'b', 'u', 'i', 'l', 'd', 'i', 'n', 'f', ':'};
    for (size_t i = 0; i + sizeof(magic) <= sz; i++)
        if (f[i] == (uint8_t)magic[0] && !memcmp(f + i, magic, sizeof magic)) return 1;
    return 0; // not a Go binary -> never suppress SIGURG for it
}

#endif
