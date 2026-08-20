//! The loader's segment-protection walk must ask the host once per contiguous run of equally
//! protected host pages, not once per page.
//!
//! A host protection change is a range operation on every supported host, so a run of adjacent
//! pages carrying the same `PT_LOAD` flags is one call. Emitting it page by page cost a
//! statically linked x86-64 guest ~181 `mprotect(2)` calls per `fork+exec` -- 36.6% of the 494
//! host syscalls the engine spends per guest spawn, measured with `strace -f -c` on `x86_64` Linux.
//!
//! This gate is a call *count*, because the count is the defect: the resulting protections were
//! always correct, and a test that only checked them would pass on the page-at-a-time form. The
//! fixture drives `hl_elf_protect_segments` against a recording host service, so it also pins
//! that coalescing changed no page's protection.

use std::{fs, path::PathBuf, process::Command};

const PROBE: &str = r#"
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>
#include "hl/host_services.h"

#define PAGES 255u
#define PAGE 4096u
#define BASE UINT64_C(0x400000)

static size_t g_host_page = PAGE;

static uint64_t applied[PAGES + 16];      /* protection recorded per page index */
static unsigned calls;                   /* host protect() invocations */

static hl_host_result record(void *context, hl_host_handle mapping, uint64_t offset, uint64_t size, uint32_t flags) {
    (void)context;
    (void)mapping;
    hl_host_result ok;
    memset(&ok, 0, sizeof ok);
    ok.status = HL_STATUS_OK;
    ++calls;
    for (uint64_t at = 0; at < size; at += PAGE) {
        uint64_t index = (offset + at) / PAGE;
        if (index < PAGES + 16) applied[index] = (uint64_t)flags + 1u;
    }
    return ok;
}

static hl_host_memory_services memory_services;
static hl_host_services services;
static const hl_host_services *effective_host_services(void) { return &services; }

/* The loader fragment keys its read-only/no-execute registries in guest coordinates and this
 * fixture places the image at its link address, so the projection is the identity. */
static uint64_t nonpie_unfold(uint64_t address) { return address; }
static void gro_add(uint64_t lo, uint64_t hi) { (void)lo; (void)hi; }
static void gro_clear(uint64_t lo, uint64_t hi) { (void)lo; (void)hi; }
static void gnx_add(uint64_t lo, uint64_t hi) { (void)lo; (void)hi; }
static void gnx_clear(uint64_t lo, uint64_t hi) { (void)lo; (void)hi; }
size_t hl_host_page_size(void) { return g_host_page; }

#include "linux_abi/elf_protect.h"

/* Three PT_LOADs in the shape every ELF has: a read-only head, an executable body, and a
 * writable tail.  The body is 253 pages, which is what a small static glibc binary carries. */
static void put32(uint8_t *p, uint32_t v) { p[0] = (uint8_t)v; p[1] = (uint8_t)(v >> 8); p[2] = (uint8_t)(v >> 16); p[3] = (uint8_t)(v >> 24); }
static void put64(uint8_t *p, uint64_t v) { put32(p, (uint32_t)v); put32(p + 4, (uint32_t)(v >> 32)); }

static uint8_t phdr[4 * 56];
static void segment(int index, uint32_t flags, uint64_t vaddr, uint64_t memsz) {
    uint8_t *e = phdr + (size_t)index * 56;
    put32(e, 1);          /* PT_LOAD */
    put32(e + 4, flags);  /* p_flags */
    put64(e + 16, vaddr); /* p_vaddr */
    put64(e + 40, memsz); /* p_memsz */
}

static void reset(void) {
    memset(applied, 0, sizeof applied);
    calls = 0;
}

/* A run of adjacent equally protected host pages is one call. */
static int contiguous_run_is_one_call(void) {
    hl_host_memory_mapping mapping;
    memset(&mapping, 0, sizeof mapping);
    mapping.address = BASE;
    g_host_page = PAGE;
    reset();

    segment(0, 4, BASE, PAGE);                        /* PF_R      1 page   */
    segment(1, 5, BASE + PAGE, (uint64_t)253 * PAGE); /* PF_R|PF_X 253 pages */
    segment(2, 6, BASE + (uint64_t)254 * PAGE, PAGE); /* PF_R|PF_W 1 page   */
    hl_elf_protect_segments(&mapping, phdr, 3, 56, 0);

    /* The last host page of the image hull is excluded by the walk's own `last` bound, so the
     * writable tail is not reached; 254 pages are protected by 2 runs. */
    if (calls > 4) { fprintf(stderr, "protect calls=%u, expected one per contiguous run\n", calls); return 1; }
    if (calls == 0) { fprintf(stderr, "the walk applied no protection at all\n"); return 2; }

    unsigned readonly = 0, executable = 0;
    for (unsigned i = 0; i < 254; i++) {
        if (applied[i] == 0) { fprintf(stderr, "page %u left unprotected\n", i); return 3; }
        uint32_t flags = (uint32_t)(applied[i] - 1u);
        if (!(flags & HL_HOST_MEMORY_READ)) { fprintf(stderr, "page %u not readable\n", i); return 4; }
        if (flags & HL_HOST_MEMORY_WRITE) { fprintf(stderr, "page %u writable\n", i); return 5; }
        if (flags & HL_HOST_MEMORY_EXECUTE) ++executable; else ++readonly;
    }
    if (readonly != 1 || executable != 253) {
        fprintf(stderr, "protections shifted: readonly=%u executable=%u\n", readonly, executable);
        return 6;
    }
    return 0;
}

/* A host page larger than the guest's is the Apple-Silicon shape: one 16 KiB host page can union
 * several 4 KiB PT_LOADs, so consecutive pages of a SINGLE segment can resolve to different flags.
 * A run must break there. Nothing on a 4 KiB host can reach this, which is why the fixture owns
 * hl_host_page_size rather than asking the host for it. */
static int a_flags_change_inside_one_segment_breaks_the_run(void) {
    hl_host_memory_mapping mapping;
    memset(&mapping, 0, sizeof mapping);
    mapping.address = BASE;
    g_host_page = 4u * PAGE;
    reset();

    segment(0, 4, BASE, 4u * (4u * PAGE));  /* PF_R over four 16 KiB host pages   */
    segment(1, 5, BASE + 4u * PAGE, PAGE);  /* PF_R|PF_X on one 4 KiB guest page  */
    segment(2, 4, BASE, 4u * (4u * PAGE));  /* padding entry: identical to seg 0  */
    hl_elf_protect_segments(&mapping, phdr, 2, 56, 0);

    for (unsigned host = 0; host < 4; host++) {
        uint64_t recorded = applied[host * 4u];
        if (recorded == 0) { fprintf(stderr, "host page %u left unprotected\n", host); return 10; }
        uint32_t flags = (uint32_t)(recorded - 1u);
        int wants_execute = host == 1;
        if (!!(flags & HL_HOST_MEMORY_EXECUTE) != wants_execute) {
            fprintf(stderr, "host page %u execute=%d, wanted %d -- a run spanned a flags change\n", host,
                    !!(flags & HL_HOST_MEMORY_EXECUTE), wants_execute);
            return 11;
        }
    }
    return 0;
}

int main(void) {
    memory_services.protect = record;
    services.memory = &memory_services;
    int verdict = contiguous_run_is_one_call();
    if (verdict != 0) return verdict;
    return a_flags_change_inside_one_segment_breaks_the_run();
}
"#;

#[test]
fn segment_protection_asks_the_host_once_per_contiguous_run() {
    let package = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let native = package.join("src/native");
    let scratch = std::env::temp_dir().join(format!("hl-native-elf-protect-{}", std::process::id()));
    fs::create_dir_all(&scratch).expect("segment protection probe directory");
    let source = scratch.join("elf_protect.c");
    let executable = scratch.join("elf_protect");
    fs::write(&source, PROBE).expect("segment protection probe source");
    let compiler = std::env::var_os("CC").unwrap_or_else(|| "cc".into());
    let built = Command::new(&compiler)
        .args(["-std=c11", "-D_GNU_SOURCE"])
        .arg(format!("-I{}", native.display()))
        .arg(format!("-I{}", native.join("include").display()))
        .arg(&source)
        .arg("-o")
        .arg(&executable)
        .status()
        .expect("compile segment protection probe");
    assert!(built.success(), "segment protection probe did not compile");
    let ran = Command::new(&executable)
        .status()
        .expect("run segment protection probe");
    assert!(ran.success(), "segment protection probe failed with {ran}");
    fs::remove_dir_all(scratch).expect("remove segment protection probe directory");
}
