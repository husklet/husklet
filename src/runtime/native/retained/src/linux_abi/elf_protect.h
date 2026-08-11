#ifndef HL_LINUX_ABI_ELF_PROTECT_H
#define HL_LINUX_ABI_ELF_PROTECT_H

#include "hl/host_services.h"

#include <stdint.h>
#include "host_mman.h"
#include <unistd.h>

// THE LOADER'S PROTECTION CONTRACT, stated once because the two loaders each holding half of it is what
// let an x86-64 guest store into its own .rodata and carry on. A PT_LOAD's p_flags decide TWO things and
// they are set together, here:
//
//   the HOST PAGE PROTECTION is the only enforcement that exists. Both engines write guest memory with a
//   plain host store or memcpy and carry no permission check of their own, so if the page is writable the
//   store lands -- there is no second gate behind it.
//
//   the READ-ONLY REGISTRY (g_gro) is what lets the resulting host fault be classified as the guest's own
//   SIGSEGV (x86.c jit86_lazyguard, elf.c nonpie_guard) rather than a page to demand-map or unprotect, and
//   is what answers /proc/self/maps and the syscall uaccess checks.
//
// Register without protecting and the store is silently dropped while every registry-reading surface
// insists the page is read-only; protect without registering and the engine cannot tell its own fault
// from a lazy-growth one. Registry keys are GUEST coordinates (thread.c's one rule) while a host
// protection takes the storage address -- hence the nonpie_unfold on the way into g_gro.
//
// This is also the state the fork server's pristine-image restore has to put back, so it calls the same
// function rather than restating the rule (fork.c, FSRV_RESTORE_DONE).

#define HL_ELF_PROTECT_RETRIES 6

static uint32_t hl_elf_ph32(const uint8_t *p) {
    return (uint32_t)p[0] | ((uint32_t)p[1] << 8) | ((uint32_t)p[2] << 16) | ((uint32_t)p[3] << 24);
}

static uint64_t hl_elf_ph64(const uint8_t *p) {
    return (uint64_t)hl_elf_ph32(p) | ((uint64_t)hl_elf_ph32(p + 4) << 32);
}

// `phdr` is the program-header table (file or mapped copy -- identical bytes), `bias` the amount the
// image was displaced from its link address. `mapping` is the image's host mapping; pass NULL to change
// the protection with mprotect(2) directly, which is all the fork-server restore has to hand.
static void hl_elf_protect_segments(const hl_host_memory_mapping *mapping, const uint8_t *phdr, int phnum, int phent,
                                    uint64_t bias) {
    size_t host_page = hl_host_page_size();
    uint64_t image_first = UINT64_MAX, image_last = 0;
    for (int i = 0; i < phnum; i++) {
        const uint8_t *ph = phdr + (size_t)i * (size_t)phent;
        if (hl_elf_ph32(ph) != 1) continue;
        uint64_t v = hl_elf_ph64(ph + 16), msz = hl_elf_ph64(ph + 40);
        uint64_t s = (v + bias) & ~UINT64_C(0xFFF);
        uint64_t e = (v + bias + msz + UINT64_C(0xFFF)) & ~UINT64_C(0xFFF);
        if (e <= s) continue;
        if (s < image_first) image_first = s;
        if (e > image_last) image_last = e;
    }
    if (host_page && image_last > image_first) {
        uint64_t first = (image_first + host_page - 1) & ~((uint64_t)host_page - 1);
        uint64_t last = image_last & ~((uint64_t)host_page - 1);
        // Walk segment coverage rather than the image hull: sparse PT_LOAD addresses must not turn their
        // unmapped gap into work or protection calls. The earliest intersecting segment owns each host page;
        // the inner scan still unions every segment sharing that page.
        for (int owner = 0; owner < phnum; ++owner) {
            const uint8_t *owner_ph = phdr + (size_t)owner * (size_t)phent;
            if (hl_elf_ph32(owner_ph) != 1) continue;
            uint64_t owner_v = hl_elf_ph64(owner_ph + 16), owner_msz = hl_elf_ph64(owner_ph + 40);
            uint64_t owner_first = (owner_v + bias) & ~UINT64_C(0xFFF);
            uint64_t owner_last = (owner_v + bias + owner_msz + UINT64_C(0xFFF)) & ~UINT64_C(0xFFF);
            if (owner_last <= owner_first) continue;
            uint64_t page = owner_first & ~((uint64_t)host_page - 1);
            if (page < first) page = first;
            for (; page < owner_last && page < last; page += host_page) {
                int already_visited = 0;
                for (int earlier = 0; earlier < owner; ++earlier) {
                    const uint8_t *earlier_ph = phdr + (size_t)earlier * (size_t)phent;
                    if (hl_elf_ph32(earlier_ph) != 1) continue;
                    uint64_t earlier_v = hl_elf_ph64(earlier_ph + 16);
                    uint64_t earlier_msz = hl_elf_ph64(earlier_ph + 40);
                    uint64_t earlier_first = (earlier_v + bias) & ~UINT64_C(0xFFF);
                    uint64_t earlier_last = (earlier_v + bias + earlier_msz + UINT64_C(0xFFF)) & ~UINT64_C(0xFFF);
                    if (earlier_last > page && earlier_first < page + host_page) {
                        already_visited = 1;
                        break;
                    }
                }
                if (already_visited) continue;
                uint32_t flags = 0;
                for (int i = 0; i < phnum; i++) {
                    const uint8_t *ph = phdr + (size_t)i * (size_t)phent;
                    if (hl_elf_ph32(ph) != 1) continue;
                    uint64_t v = hl_elf_ph64(ph + 16), msz = hl_elf_ph64(ph + 40);
                    uint64_t s = (v + bias) & ~UINT64_C(0xFFF);
                    uint64_t e = (v + bias + msz + UINT64_C(0xFFF)) & ~UINT64_C(0xFFF);
                    if (e > page && s < page + host_page) flags |= hl_elf_ph32(ph + 4);
                }
                if (flags == 0) continue;
                if (mapping != NULL) {
                    uint32_t protection = HL_HOST_MEMORY_READ | ((flags & 2) ? HL_HOST_MEMORY_WRITE : 0) |
                                          ((flags & 1) ? HL_HOST_MEMORY_EXECUTE : 0);
                    const hl_host_services *host = effective_host_services();
                    for (int t = 0;; t++) {
                        hl_host_result r = host->memory->protect(host->context, mapping->handle,
                                                                 page - mapping->address, host_page, protection);
                        if (r.status == HL_STATUS_OK || r.status != HL_STATUS_OUT_OF_MEMORY ||
                            t >= HL_ELF_PROTECT_RETRIES)
                            break;
                        usleep(2000u << t);
                    }
                } else {
                    int protection = PROT_READ | ((flags & 2) ? PROT_WRITE : 0) | ((flags & 1) ? PROT_EXEC : 0);
                    (void)mprotect((void *)(uintptr_t)page, host_page, protection);
                }
            }
        }
    }
    for (int i = 0; i < phnum; i++) {
        const uint8_t *ph = phdr + (size_t)i * (size_t)phent;
        if (hl_elf_ph32(ph) != 1) continue; // PT_LOAD
        uint32_t fl = hl_elf_ph32(ph + 4);  // PF_X=1, PF_W=2, PF_R=4
        uint64_t v = hl_elf_ph64(ph + 16), msz = hl_elf_ph64(ph + 40);
        uint64_t s = (v + bias) & ~0xFFFull, e = (v + bias + msz + 0xFFFull) & ~0xFFFull;
        if (e <= s) continue;
        uint64_t gs = nonpie_unfold(s), ge = nonpie_unfold(e - 1) + 1;
        if (fl & 2) {
            gro_clear(gs, ge);
        } else {
            gro_add(gs, ge);
        }
    }
}

#endif
