#ifndef HL_LINUX_ABI_ELF_PROTECT_H
#define HL_LINUX_ABI_ELF_PROTECT_H

// Target composition fragment: loaders include this only after defining their private address-projection,
// read-only-registry, and effective-host-service helpers.

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
#define HL_ELF_PROTECT_RETRIES 6

static uint32_t hl_elf_ph32(const uint8_t *p) {
    return (uint32_t)p[0] | ((uint32_t)p[1] << 8) | ((uint32_t)p[2] << 16) | ((uint32_t)p[3] << 24);
}

static uint64_t hl_elf_ph64(const uint8_t *p) {
    return (uint64_t)hl_elf_ph32(p) | ((uint64_t)hl_elf_ph32(p + 4) << 32);
}

// One protection change over [address, address + length), retrying only the transient out-of-memory the
// host reports when it cannot split a VM entry under pressure. Returns non-zero when the host accepted it.
static int hl_elf_protect_span(const hl_host_memory_mapping *mapping, uint64_t address, uint64_t length,
                               uint32_t flags) {
    if (mapping != NULL) {
        uint32_t protection = HL_HOST_MEMORY_READ | ((flags & 2) ? HL_HOST_MEMORY_WRITE : 0) |
                              ((flags & 1) ? HL_HOST_MEMORY_EXECUTE : 0);
        const hl_host_services *host = effective_host_services();
        for (int t = 0;; t++) {
            hl_host_result r =
                host->memory->protect(host->context, mapping->handle, address - mapping->address, length, protection);
            if (r.status == HL_STATUS_OK) return 1;
            if (r.status != HL_STATUS_OUT_OF_MEMORY || t >= HL_ELF_PROTECT_RETRIES) return 0;
            usleep(2000u << t);
        }
    }
    {
        int protection = PROT_READ | ((flags & 2) ? PROT_WRITE : 0) | ((flags & 1) ? PROT_EXEC : 0);
        return mprotect((void *)(uintptr_t)address, (size_t)length, protection) == 0;
    }
}

// Apply one coalesced run. A refusal of the whole run falls back to the page-at-a-time form it replaced,
// so a host that can tighten some pages of a span and not others still tightens exactly those pages --
// the loader's protection contract is best-effort per page and coalescing must not narrow it.
static void hl_elf_protect_run(const hl_host_memory_mapping *mapping, uint64_t address, uint64_t length,
                               uint32_t flags, size_t host_page) {
    if (length == 0 || flags == 0) return;
    if (hl_elf_protect_span(mapping, address, length, flags)) return;
    if (length == (uint64_t)host_page) return; /* a one-page run has no narrower form to retry */
    for (uint64_t page = address; page < address + length; page += host_page)
        (void)hl_elf_protect_span(mapping, page, (uint64_t)host_page, flags);
}

// `phdr` is the program-header table (file or mapped copy -- identical bytes), `bias` the amount the
// image was displaced from its link address. `mapping` is the image's host mapping; pass NULL to change
// the protection with mprotect(2) directly.
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
        uint64_t run_start = 0;
        uint64_t run_length = 0;
        uint32_t run_flags = 0;
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
                uint32_t flags = 0;
                if (!already_visited) {
                    for (int i = 0; i < phnum; i++) {
                        const uint8_t *ph = phdr + (size_t)i * (size_t)phent;
                        if (hl_elf_ph32(ph) != 1) continue;
                        uint64_t v = hl_elf_ph64(ph + 16), msz = hl_elf_ph64(ph + 40);
                        uint64_t s = (v + bias) & ~UINT64_C(0xFFF);
                        uint64_t e = (v + bias + msz + UINT64_C(0xFFF)) & ~UINT64_C(0xFFF);
                        if (e > page && s < page + host_page) flags |= hl_elf_ph32(ph + 4);
                    }
                }
                /* A protection change is per-range, not per-page, on every supported host, and a run of
                 * adjacent host pages that resolved to the SAME PT_LOAD flags is exactly such a range.
                 * Emitting it page by page asked the host kernel once per page for an answer one call
                 * covers: a statically linked x86-64 guest carries ~255 executable pages, so a single
                 * fork+exec issued ~181 mprotect(2) calls where three suffice -- 36.6% of the 494
                 * host syscalls this engine spends per guest spawn (measured, naa0245, x86_64 Linux).
                 * Defer each page into the open run and flush when the run cannot grow. */
                if (run_length != 0 && (flags != run_flags || page != run_start + run_length))
                    hl_elf_protect_run(mapping, run_start, run_length, run_flags, host_page);
                if (flags == 0) {
                    run_length = 0;
                    continue;
                }
                if (run_length != 0 && flags == run_flags && page == run_start + run_length) {
                    run_length += host_page;
                } else {
                    run_start = page;
                    run_length = host_page;
                    run_flags = flags;
                }
            }
            if (run_length != 0) hl_elf_protect_run(mapping, run_start, run_length, run_flags, host_page);
            run_length = 0;
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
        if (fl & 1)
            gnx_clear(gs, ge);
        else
            gnx_add(gs, ge);
    }
}

#endif
