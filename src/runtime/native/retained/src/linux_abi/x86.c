// hl/linux_abi -- x86-64 ELF loader (ET_EXEC at its link address on Linux, biased high on macOS;
// static-PIE + dynamic via ld.so) + stack. Placement rule: thread.c, "non-PIE image placement, per host".
#include "placement.h"

struct elf_host_map_context {
    hl_host_memory_mapping mapping;
};

static void *elf_host_map(void *context, void *address, size_t length, uint32_t placement) {
    struct elf_host_map_context *state = context;
    const hl_host_services *host = effective_host_services();
    uint32_t flags = HL_HOST_MEMORY_PRIVATE;
    if (placement == HL_ELF_MAP_FIXED) flags |= HL_HOST_MEMORY_FIXED;
    state->mapping = (hl_host_memory_mapping){HL_HOST_MEMORY_MAPPING_ABI, sizeof(state->mapping), 0, 0, 0, 0};
    hl_host_result result =
        host->memory->map_anonymous(host->context, (uint64_t)(uintptr_t)address, length,
                                    HL_HOST_MEMORY_READ | HL_HOST_MEMORY_WRITE, flags, &state->mapping);
    return result.status == HL_STATUS_OK ? (void *)(uintptr_t)state->mapping.address : NULL;
}

#include <stdatomic.h>
/* <sys/ucontext.h> only where a ucontext_t comes from a system header. On a host
 * with no signals there is no such header and no such type: native_context.h's
 * cell for that host names the register file the fault primitive delivers
 * instead, so it is the only include that is unconditional here. */
#if !defined(_WIN32)
#include <sys/ucontext.h>
#endif
#include "../host/native_context.h"

#include "../host/range.h"
#include "page.h"
#include "elf_protect.h" // the loader's protection contract, shared with linux_abi/elf.c
#include "image.h"
#include "../translator/guest/x86_64/cpuid.h" // AT_HWCAP derives from the guest CPUID model, not the host

static int x86_image_read(const char *path, hl_linux_image *image) {
    if (g_initial_executable_image != NULL)
        return hl_linux_image_read_bytes(g_initial_executable_image, g_initial_executable_size, image);
    if (g_authorized_executable_image != NULL && path != NULL && g_authorized_executable_path[0]) {
        char canonical[4200];
        if (realpath(path, canonical) != NULL && strcmp(canonical, g_authorized_executable_path) == 0)
            return hl_linux_image_read_bytes(g_authorized_executable_image, g_authorized_executable_size, image);
    }
    if (g_rootfs == NULL) return hl_linux_image_read(effective_host_services(), path, image);
    char guest[4200];
    const char *request = path;
    if (path != NULL && path[0] == '/') {
        int backing = g_rootfs && !strncmp(path, g_rootfs_canon, g_rootfs_canon_len) &&
                      (path[g_rootfs_canon_len] == 0 || path[g_rootfs_canon_len] == '/');
        for (int volume = 0; !backing && volume < g_nvols; ++volume)
            backing = !strncmp(path, g_vols[volume].hcanon, g_vols[volume].hlen) &&
                      (path[g_vols[volume].hlen] == 0 || path[g_vols[volume].hlen] == '/');
        for (int lower = 0; !backing && lower < g_nlower; ++lower) {
            if (!strncmp(path, g_lower[lower].canon, g_lower[lower].clen) &&
                (path[g_lower[lower].clen] == 0 || path[g_lower[lower].clen] == '/')) {
                const char *suffix = path + g_lower[lower].clen;
                snprintf(guest, sizeof guest, "%s", suffix[0] ? suffix : "/");
                request = guest;
                backing = 2;
            }
        }
        if (backing == 1) {
            int mapped = guest_from_host_raw(path, guest, sizeof guest);
            if (mapped <= 0) {
                errno = mapped < 0 ? -mapped : EACCES;
                return -1;
            }
            request = guest;
        }
    }
    if (request != NULL && request[0] == '/' && (g_rootfs != NULL || jail_match(request) >= 0)) {
        if (g_nlower) {
            char backing[4200];
            /* Executable lookup follows the final symlink.  overlay_lookup is the
               no-follow primitive used by lstat/readlink and cannot be used with
               the O_NOFOLLOW ELF open for paths such as /bin/python. */
            if (!overlay_resolve(request, backing, sizeof backing, 0)) return -1;
            int descriptor = open(backing, O_RDONLY | O_CLOEXEC | O_NOFOLLOW);
            int result = descriptor < 0 ? -1 : hl_linux_image_read_fd(descriptor, image);
            if (descriptor >= 0) close(descriptor);
            return result;
        }
        char final[512];
        int directory = jail_at(-100, request, final, sizeof final, 0);
        if (directory < 0) return -1;
        int descriptor = openat(directory, final, O_RDONLY | O_CLOEXEC | O_NOFOLLOW);
        int result = descriptor < 0 ? -1 : hl_linux_image_read_fd(descriptor, image);
        if (descriptor >= 0) close(descriptor);
        close(directory);
        return result;
    }
    return hl_linux_image_read(effective_host_services(), request, image);
}

// ---------------- minimal ELF loader (load high; copied from jit.c) ----------------
static uint16_t rd16(const uint8_t *p) {
    return p[0] | (p[1] << 8);
}

static uint32_t rd32(const uint8_t *p) {
    uint32_t v;
    memcpy(&v, p, 4);
    return v;
}

static uint64_t rd64(const uint8_t *p) {
    uint64_t v;
    memcpy(&v, p, 8);
    return v;
}

static void wr64(uint8_t *p, uint64_t v) {
    memcpy(p, &v, 8);
}

// struct loaded is defined by the shared os/linux (container/netns.c).

static int elf_interp(const char *path, char *out, size_t n) {
    hl_linux_image image;
    if (x86_image_read(path, &image) != 0) return -1;
    uint8_t *f = image.bytes;
    int r = -1;
    uint64_t phoff = rd64(f + 32);
    int phnum = rd16(f + 56), phent = rd16(f + 54);
    for (int i = 0; i < phnum; i++) {
        const uint8_t *ph = f + phoff + (size_t)i * phent;
        if (rd32(ph) == 3) {
            uint64_t off = rd64(ph + 8), fsz = rd64(ph + 32);
            size_t l = fsz < n ? fsz : n - 1;
            memcpy(out, f + off, l);
            out[l] = 0;
            r = 0;
            break;
        }
    }
    hl_linux_image_release(&image);
    return r;
}

// W6A item 1 (Go non-PIE): a Go ET_EXEC's runtime keeps its OWN code/data addresses in the
// firstmoduledata struct (and the pclntab it points at) as LINK-TIME absolute values: text/etext,
// minpc/maxpc, the pclntab slice pointers, findfunctab, the type/gc/gofunc bases, the init-task
// list, etc. After we bias the image HIGH (macOS reserves the low 4GB), the runtime's live code PCs
// -- return addresses pushed by `call`, function pointers materialized by rip-relative `lea` -- are
// all BIASED, but findfunc() still compares them against the un-biased moduledata: findmoduledatap()
// rejects the pc (pc >= maxpc), findfunc() returns a nil funcInfo, and runtime.pcdatavalue derefs
// it -> SIGSEGV at offset 0x1c. Fix: at load time, add g_nonpie_bias to every ABSOLUTE pointer word
// of firstmoduledata so the comparisons line up with the biased PCs. The pclntab's own tables are
// pc-DELTAS and text-RELATIVE offsets, EXCEPT pcHeader.textStart (Go 1.18+) which is an absolute
// address that moduledataverify1 asserts equals moduledata.text -- so the moduledata pointer words,
// pcHeader.textStart and the textsect baseaddr are all rebased; slice len/cap, relative offsets and
// flags are left untouched. Each rebase is guarded to the link range [lo,hi) so a zero/small/already-
// mapped field is never disturbed. The module is located via the runtime.firstmoduledata symbol, or
// (a stripped image, e.g. etcd/traefik) by scanning the data sections for the &pcHeader signature, and
// validated by the pclntab magic -> a no-op for a non-Go ET_EXEC. Layout per Go runtime/symtab.go
// (verified against the go1.26 nats-server).
// Look up a symbol's st_value in the ELF .symtab (+ its linked string table). Returns 0 if absent.
static uint64_t go_symval(const uint8_t *f, size_t fsz, const char *name) {
    uint64_t shoff = rd64(f + 0x28);
    int shnum = rd16(f + 0x3C), shent = rd16(f + 0x3A);
    if (!shoff || !shent || (uint64_t)shoff + (uint64_t)shnum * shent > fsz) return 0;
    for (int i = 0; i < shnum; i++) {
        const uint8_t *sh = f + shoff + (uint64_t)i * shent;
        if (rd32(sh + 4) != 2) continue; // SHT_SYMTAB
        uint64_t symoff = rd64(sh + 0x18), symsz = rd64(sh + 0x20), syment = rd64(sh + 0x38);
        uint32_t strndx = rd32(sh + 0x28); // sh_link -> string table section
        if (!syment || strndx >= (uint32_t)shnum) continue;
        const uint8_t *strsh = f + shoff + (uint64_t)strndx * shent;
        uint64_t stroff = rd64(strsh + 0x18), strsz = rd64(strsh + 0x20);
        if (symoff + symsz > fsz || stroff + strsz > fsz) continue;
        for (uint64_t o = 0; o + syment <= symsz; o += syment) {
            const uint8_t *sym = f + symoff + o;
            uint32_t nameoff = rd32(sym);
            if (nameoff < strsz && strcmp((const char *)(f + stroff + nameoff), name) == 0) return rd64(sym + 8);
        }
    }
    return 0;
}

// Find a section by name via the .shstrtab (e_shstrndx). Returns its header (and sh_addr/off/size out
// params) or NULL. Section headers + names survive `-ldflags=-s -w` even though .symtab does not.
static const uint8_t *go_section_by_name(const uint8_t *f, size_t fsz, const char *name, uint64_t *sh_addr,
                                         uint64_t *sh_off, uint64_t *sh_size) {
    uint64_t shoff = rd64(f + 0x28);
    int shnum = rd16(f + 0x3C), shent = rd16(f + 0x3A), shstrndx = rd16(f + 0x3E);
    if (!shoff || !shent || shstrndx >= shnum || (uint64_t)shoff + (uint64_t)shnum * shent > fsz) return NULL;
    const uint8_t *strsh = f + shoff + (uint64_t)shstrndx * shent;
    uint64_t stroff = rd64(strsh + 0x18), strsz = rd64(strsh + 0x20);
    if (stroff + strsz > fsz) return NULL;
    for (int i = 0; i < shnum; i++) {
        const uint8_t *sh = f + shoff + (uint64_t)i * shent;
        uint32_t nameoff = rd32(sh);
        if (nameoff < strsz && strcmp((const char *)(f + stroff + nameoff), name) == 0) {
            if (sh_addr) *sh_addr = rd64(sh + 0x10);
            if (sh_off) *sh_off = rd64(sh + 0x18);
            if (sh_size) *sh_size = rd64(sh + 0x20);
            return sh;
        }
    }
    return NULL;
}

// Locate firstmoduledata's link-time vaddr. Prefer the runtime.firstmoduledata symbol; for a STRIPPED
// image (no .symtab -- the common production case: etcd, traefik, caddy build with `-s -w`) fall back to
// scanning the data sections. firstmoduledata's first word is &pcHeader, which equals the .gopclntab
// section base; a candidate is accepted only when minpc/maxpc lie in the link range and bracket text
// (and, when pcHeader carries an absolute textStart, text matches it) -- a signature strong enough to
// reject stray hits.
static uint64_t go_find_moduledata(const uint8_t *f, size_t fsz, uint64_t lo, uint64_t hi) {
    uint64_t md = go_symval(f, fsz, "runtime.firstmoduledata");
    if (md >= lo && md < hi) return md;
    uint64_t pcva = 0, pcoff = 0;
    if (!go_section_by_name(f, fsz, ".gopclntab", &pcva, &pcoff, NULL) || !pcva || pcoff + 32 > fsz) return 0;
    uint32_t magic = rd32(f + pcoff);
    // pcHeader.textStart (offset 24, Go 1.18+) is the absolute text base on Go <=1.23 (etcd/traefik) but a
    // reserved 0 on Go 1.24+ (caddy, go1.26). Only use it as a moduledata cross-check when it is a real
    // in-range link address; a zero textStart just means we lean on the other signature fields.
    uint64_t textstart = (magic == 0xfffffff0u || magic == 0xfffffff1u) ? rd64(f + pcoff + 24) : 0;
    int ts_check = textstart >= lo && textstart < hi;
    uint64_t shoff = rd64(f + 0x28);
    int shnum = rd16(f + 0x3C), shent = rd16(f + 0x3A);
    if (!shoff || !shent || (uint64_t)shoff + (uint64_t)shnum * shent > fsz) return 0;
    for (int i = 0; i < shnum; i++) {
        const uint8_t *sh = f + shoff + (uint64_t)i * shent;
        if (rd32(sh + 4) != 1) continue; // SHT_PROGBITS (data with file content)
        uint64_t sa = rd64(sh + 0x10), so = rd64(sh + 0x18), ssz = rd64(sh + 0x20);
        if (!sa || so + ssz > fsz) continue;
        for (uint64_t o = 0; o + 8 <= ssz; o += 8) {
            if (rd64(f + so + o) != pcva) continue; // word 0 == &pcHeader
            uint64_t coff = so + o;
            if (coff + 23 * 8 > fsz) continue; // need words up to text(22)
            uint64_t text = rd64(f + coff + 22 * 8), minpc = rd64(f + coff + 20 * 8), maxpc = rd64(f + coff + 21 * 8);
            if (text >= lo && text < hi && minpc >= lo && maxpc <= hi && text <= minpc && minpc < maxpc &&
                (!ts_check || text == textstart))
                return sa + o;
        }
    }
    return 0;
}

static void go_rebase_nonpie(const uint8_t *f, size_t fsz, uint64_t bias, uint64_t lo, uint64_t hi) {
    uint64_t md_va = go_find_moduledata(f, fsz, lo, hi);
    if (!md_va || md_va < lo || md_va >= hi) return;
    uint8_t *md = (uint8_t *)(md_va + bias); // the mapped (biased) copy of firstmoduledata
    // Validate: field 0 is &pclntab, whose first u32 is the Go pclntab magic. Bail if not Go.
    uint64_t pch = rd64(md);
    if (pch < lo || pch >= hi) return;
    uint32_t magic = rd32((const uint8_t *)(pch + bias));
    if (magic != 0xfffffff0u && magic != 0xfffffff1u && magic != 0xfffffffau && magic != 0xfffffffbu) return;
    // moduledataverify1 asserts pcHeader.textStart == moduledata.text. We rebase moduledata.text HIGH
    // (ptr_word 22 below) so live PCs resolve via findfunc, so pcHeader.textStart -- an ABSOLUTE address
    // baked at the link-time text base, at offset 24 of pcHeader (Go 1.18+, magic f0/f1; the older fa/fb
    // pclntab predates the field) -- must move high in lockstep, else the runtime aborts with
    // "invalid function symbol table" (pcHeader.textStart != text).
    if (magic == 0xfffffff0u || magic == 0xfffffff1u) {
        uint8_t *pchdr = (uint8_t *)(pch + bias);
        uint64_t ts = rd64(pchdr + 24);
        if (ts >= lo && ts < hi) wr64(pchdr + 24, ts + bias);
    }
    // Absolute pointer words of moduledata, in 8-byte units (Go 1.26 runtime/symtab.go). Slice headers
    // contribute only their .ptr word; len/cap follow and are skipped. The guard below also skips any
    // word that is zero or outside the link range, so unused fields cost nothing.
    // Only the words that name CODE PCs / live read bases / GC segment bounds are rebased high: minpc/maxpc
    // and text are compared against the high return-address PCs; the pcln tables, findfunctab, data/bss
    // bounds and gc masks are dereferenced as mapped memory by findfunc/the GC. The type-system bases
    // (types,etypes,rodata,gofunc -- words 37..40) are deliberately LEFT LOW: type/data pointers materialized
    // by rip-relative lea are rewritten low (translate.c) to match the image's baked-absolute low pointers,
    // so findmoduledatap's `types <= p < etypes` range check and resolveTypeOff must use the low bases too
    // (rebasing them high made Go's type identity -- e.g. runtime.SetFinalizer's `fint == etyp` -- diverge).
    // Any low type/data access is served by nonpie_fixup at +bias.
    static const int ptr_words[] = {
        0,                              // pcHeader
        1,  4,  7,  10, 13, 16,         // funcnametab/cutab/filetab/pctab/pclntable/ftab slice ptrs
        19, 20, 21,                     // findfunctab, minpc, maxpc
        22, 23, 24, 25, 26, 27, 28, 29, // text,etext,noptrdata,enoptrdata,data,edata,bss,ebss
        30, 31, 32, 33, 34, 35, 36,     // noptrbss,enoptrbss,covctrs,ecovctrs,end,gcdata,gcbss
        41,                             // epclntab (types,etypes,rodata,gofunc = 37..40 stay low)
        42, 45, 48, 51,                 // textsectmap,typelinks,itablinks,ptab slice ptrs
        54, 56, 59, 62, 64,             // pluginpath,pkghashes,inittasks,modulename,modulehashes ptrs
        69, 71, 72, 73,                 // gcdatamask.bytedata, gcbssmask.bytedata, typemap, next
    };
    for (size_t k = 0; k < sizeof ptr_words / sizeof *ptr_words; k++) {
        uint8_t *slot = md + (size_t)ptr_words[k] * 8;
        uint64_t cur = rd64(slot);
        if (cur >= lo && cur < hi) wr64(slot, cur + bias);
    }
    // textsectmap is []textsect{vaddr, end, baseaddr}; only baseaddr is an absolute (relocated) address
    // -- vaddr/end are text-relative -- so rebase each entry's baseaddr explicitly. (With a single text
    // section the runtime ignores baseaddr, but keep it consistent for the multi-section case.)
    uint64_t ts_ptr = rd64(md + 42 * 8), ts_len = rd64(md + 43 * 8); // ts_ptr already rebased above
    if (ts_ptr >= lo + bias && ts_ptr < hi + bias) {
        for (uint64_t i = 0; i < ts_len && i < 64; i++) {
            uint8_t *ba = (uint8_t *)(ts_ptr + i * 24 + 16);
            uint64_t cur = rd64(ba);
            if (cur >= lo && cur < hi) wr64(ba, cur + bias);
        }
    }
    // W6A item 1: runtime.lastmoduledatap is a global *moduledata holding the baked-absolute (low) address
    // of firstmoduledata. The runtime compares it against &firstmoduledata taken by a rip-relative lea --
    // which materializes the HIGH mapped address (firstmoduledata is in the writable data segment, outside
    // the type section, so its lea is NOT rewritten low). A low lastmoduledatap vs high &firstmoduledata
    // makes runtime.main's `for md := &firstmoduledata; ...; md = md.next` loop overrun the single module
    // (md never equals lastmoduledatap) and dereference md.next == nil -> SIGSEGV. Rebase the pointer the
    // global holds to its high mapping so the identity holds. (modulesSlice entries stay low: they are only
    // DEREFERENCED -- served by nonpie_fixup -- never compared against a lea.)
    uint64_t lmdp_va = go_symval(f, fsz, "runtime.lastmoduledatap");
    if (lmdp_va >= lo && lmdp_va < hi) {
        uint8_t *slot = (uint8_t *)(lmdp_va + bias);
        uint64_t cur = rd64(slot);
        if (cur >= lo && cur < hi) wr64(slot, cur + bias);
    }
    // W6A item 1: publish the (left-low) type section [types, etypes) -- moduledata words 37,38, which are
    // deliberately NOT rebased above. translate.c rewrites a rip-relative lea whose target lands here to the
    // low link address so lea-built *_type pointers match the image's baked-absolute (low) type pointers and
    // Go's type identity holds. Only set when both bounds are sane low link addresses.
    uint64_t tlo = rd64(md + 37 * 8), thi = rd64(md + 38 * 8);
    if (tlo >= lo && thi <= hi && tlo < thi) {
        g_nonpie_types_lo = tlo;
        g_nonpie_types_hi = thi;
    }
    extern int g_diag;
    if (g_trace || g_diag)
        fprintf(stderr, "[go-rebase] firstmoduledata@%llx +bias=%llx (magic=%x) types=[%llx,%llx)\n",
                (unsigned long long)md_va, (unsigned long long)bias, magic, (unsigned long long)g_nonpie_types_lo,
                (unsigned long long)g_nonpie_types_hi);
}

struct main_placement {
    uint64_t link_start;
    uint64_t link_end;
    int etype;
};

static int main_placement_from_plan(const hl_engine_main_image_plan *plan, struct main_placement *placement) {
    if (plan == NULL || placement == NULL || plan->abi != HL_ENGINE_MAIN_IMAGE_PLAN_ABI || plan->size < sizeof(*plan) ||
        plan->link_end <= plan->link_start || (plan->kind != 1 && plan->kind != 2))
        return -1;
    placement->link_start = plan->link_start;
    placement->link_end = plan->link_end;
    placement->etype = plan->kind == 1 ? 2 : 3;
    return 0;
}

static void load_elf(const char *path, struct loaded *out, const void *placement_argument) {
    const struct main_placement *placement = placement_argument;
    hl_linux_image image;
    if (x86_image_read(path, &image) != 0) {
        fprintf(stderr, "hl-engine: cannot read guest ELF %s through host services\n", path);
        exit(1);
    }
    uint8_t *f = image.bytes;
    if (rd16(f + 18) != 0x3E) fprintf(stderr, "[hl] warning: e_machine=%u (want 62=x86-64)\n", rd16(f + 18));
    uint64_t e_entry = rd64(f + 24), phoff = rd64(f + 32);
    int phnum = rd16(f + 56), phentsize = rd16(f + 54);
    uint64_t basepage, span;
    int etype;
    if (placement != NULL) {
        basepage = placement->link_start;
        span = placement->link_end - placement->link_start;
        etype = placement->etype;
    } else {
        uint64_t minv = ~0ull, maxv = 0;
        for (int i = 0; i < phnum; i++) {
            uint8_t *ph = f + phoff + (uint64_t)i * phentsize;
            if (rd32(ph) != 1) continue;
            uint64_t v = rd64(ph + 16), msz = rd64(ph + 40);
            if (v < minv) minv = v;
            if (v + msz > maxv) maxv = v + msz;
        }
        basepage = minv & ~0xFFFull;
        span = (maxv - basepage + 0xFFFF) & ~0xFFFFull;
        etype = rd16(f + 16);
    }
    struct elf_host_map_context map_context = {
        .mapping = {HL_HOST_MEMORY_MAPPING_ABI, sizeof(hl_host_memory_mapping), 0, 0, 0, 0}};
    uint8_t *base = NULL;
    // Placement: thread.c, "non-PIE image placement, per host". On Linux an ET_EXEC goes AT its link
    // address, so bias == 0 and the whole fold family below never arms. That address is also already
    // deterministic across runs, which is the only thing g_force_base exists to provide -- consume it.
    if (etype == 2 && !nonpie_force_displaced())
        base = (uint8_t *)(uintptr_t)nonpie_place_at_link_address(basepage, span, &map_context.mapping);
    // opt8: the persistent cache needs deterministic guest bases across runs so the translated bytes
    // (RIP-relative leas, baked branch targets, block-map keys) are byte-identical. When g_force_base is
    // set, map MAP_FIXED at the caller-requested address; the image is PIE so basepage is ~0 and the chosen
    // base becomes out->base, deriving all guest PCs/addresses identically each run. One-shot per image.
    if (base != NULL) {
        g_force_base = 0; // one-shot, consumed by the link-address placement
    } else if (g_force_base) {
        void *want = (void *)(g_force_base + basepage);
        int fixed_failed;
        g_force_base = 0; // one-shot: consumed for THIS load
        // #210: the requested fixed VA can already be occupied -- a prior mapping (the interp vs the main
        // image both want deterministic bases), an ASLR collision, or 16KiB-host vs 4KiB-guest page
        // rounding leaving PC_IMG_BASE/PC_INTERP_BASE straddling a live entry. MAP_FIXED then returns
        // MAP_FAILED (macOS won't overwrite a live VM entry). Do NOT exit(1): retry at a kernel-chosen base
        // (byte-exact execution, just not cache-revivable this run) and latch g_force_base_failed so the
        // pcache neither restores a fixed-base file over this now-mixed-base arena nor persists one. This
        // matches the aarch64 loader fallback (linux_abi/elf.c) + its g_force_base_failed pcache gate.
        base = hl_elf_place_image(elf_host_map, &map_context, want, span, &fixed_failed);
        if (fixed_failed) {
            g_force_base_failed = 1;
        } else {
            // record the fixed image's live guest span -- the pcache's save/restore policy keys
            // "revivable by identity" off these ranges (guest/x86_64/cache.c pcache_note_fixed_img).
            pcache_note_fixed_img((uint64_t)base, span);
        }
    } else
        base = hl_elf_place_image(elf_host_map, &map_context, NULL, span, NULL);
    if (base == NULL) {
        fprintf(stderr, "hl-engine: load_elf: cannot map x86 guest image (%llu bytes)\n", (unsigned long long)span);
        exit(1);
    }
    if (hl_exec_mapping_add((uint64_t)base, span, map_context.mapping.handle) != 0) {
        (void)effective_host_services()->memory->release(effective_host_services()->context,
                                                         map_context.mapping.handle);
        fprintf(stderr, "hl-engine: loader mapping registry exhausted\n");
        exit(1);
    }
    hl_gmap_add((uint64_t)base, span);
    uint64_t bias = (uint64_t)base - basepage;
    // W6A item 1: a non-PIE ET_EXEC's un-relocated ABSOLUTE refs name its link vaddr, so whenever the
    // loader could not place it there (macOS __PAGEZERO) those refs land on an unmapped address. Record
    // the link range + bias so the dispatcher can redirect absolute CODE jumps and the SIGSEGV handler
    // (nonpie_fixup) can serve absolute DATA loads/stores at +bias. Armed on `bias != 0`, not on
    // `etype == 2`: a link-address placement (the Linux path above) has one coordinate system, and
    // leaving lo/hi set with bias 0 would keep every workaround below reachable for no reason.
    // NONPIE_NOFIXUP=1 disables (legacy: code jump still faults on the low vaddr). g_nonpie_* live in the
    // shared os/linux/container/vfs.c; service.c resets them across execve (case 221) for re-loaded images.
    if (etype == 2 && bias != 0) {
        g_nonpie_lo = basepage;
        g_nonpie_hi = basepage + span;
        g_nonpie_bias = bias;
        g_nonpie_types_lo = g_nonpie_types_hi = 0; // set by go_rebase_nonpie iff this is a Go image
    }
    if (etype == 2 && nonpie_force_displaced())
        nonpie_report_forced_displacement(basepage, basepage + span, (uint64_t)base);
    for (int i = 0; i < phnum; i++) {
        uint8_t *ph = f + phoff + (uint64_t)i * phentsize;
        if (rd32(ph) != 1) continue;
        uint64_t off = rd64(ph + 8), v = rd64(ph + 16), fsz = rd64(ph + 32);
        memcpy((void *)(v + bias), f + off, fsz);
    }
    // W6A item 1: for a biased non-PIE Go image, rebase firstmoduledata so the runtime's findfunc()
    // resolves the biased code PCs (otherwise runtime.pcdatavalue nil-derefs). Gated on g_nonpie_lo
    // (ET_EXEC only); NOGOREBASE=1 disables for A/B testing.
    if (g_nonpie_lo) go_rebase_nonpie(f, image.size, bias, g_nonpie_lo, g_nonpie_hi);
    // record V8's embedded-builtins CODE base symbol (LOW link value) so the frontend can bias its one
    // baked `mov r32,imm` materialization to the high mapping -- see translate.c g_nonpie_blob_code. Only for a
    // biased non-PIE image that actually carries the symbol (node/mongosh/any embedded-V8 ET_EXEC); 0 otherwise
    // (PIE, Go, stripped, non-V8) -> inert. NOV8BLOB=1 disables for A/B.
    // Gate on THIS image being the BIASED non-PIE ET_EXEC, not on the persistent g_nonpie_lo: the
    // interpreter (ld.so, a DYN loaded by a SECOND load_elf in the same process) has no v8 symbol and would
    // otherwise reset the value the main image just recorded. Only the main non-PIE exe carries the blob.
    if (etype == 2 && bias != 0) g_nonpie_blob_code = go_symval(f, image.size, "v8_Default_embedded_blob_code_");
    // a biased non-PIE ET_EXEC (e.g. static glibc jq) carries baked ABSOLUTE pointers in
    // .data.rel.ro AND .data (pointer tables) that the static linker resolved to LINK addresses with NO
    // runtime relocation entry. After we bias the image high (macOS __PAGEZERO blocks the low link range)
    // those pointers still point at the unmapped link addresses while rip-relative leas to the SAME
    // objects ARE biased -- the inconsistency makes glibc free() a static rodata pointer -> munmap_chunk
    // abort (jq --version; same class as the gcc-ld / git / rustc SIGSEGVs). Re-relocate every 8-byte word
    // in those sections whose value lands in the original link range by +bias (what an R_X86_64_RELATIVE
    // would do). .data is mixed, so a non-pointer qword that happens to fall in [lo,hi) is a (rare) false
    // positive -- which is why a Linux host maps the image AT its link address instead and never gets
    // here. Gated by NORELRO=1 for A/B. Biased ET_EXEC only; static-PIE carries real relocs, never here.
    // Skip GO binaries: go_rebase_nonpie above already rebased their moduledata/.data pointers; a blind
    // .data scan here double-biases the Go name/type tables (etcd -> "nameOff ... not in ranges"). Detect
    // Go via .gopclntab (present in every Go binary, stripped or not).
    // Restrict the blind .data rebasing to STATIC non-PIE images (no PT_INTERP). A static binary (musl jq,
    // busybox) has no ld.so and its baked-absolute .data pointers are compared against rip-relative-lea
    // HIGH values, so the words must move HIGH too. A DYNAMIC non-PIE (glibc gcc/cc1 driver) instead
    // materializes those same pointers as LOW link addresses in code (mov-imm / data loads that hl's
    // ea_bias17 folds on access), so it compares LOW==LOW natively -- rebasing its words HIGH is what broke
    // gcc's set_static_spec pointer-identity check (gcc_unreachable ICE). Gating on static cleanly separates
    // the two: jq/busybox stay rebased, gcc/cc1 stay low-consistent.
    //
    // Preserve links whose slot and target are in the SAME section. These are guest identity links in
    // mutable/runtime structures, not host pointers: glibc's main_arena.next is the canonical example.
    // Rebasing that self-link while a RIP-relative LEA still produces the low guest address breaks the
    // circular-list sentinel and makes fork lock main_arena twice. Cross-section pointers retain the
    // historical relocation needed by static jq/busybox pointer tables.
    int has_interp = 0;
    for (int i = 0; i < phnum; i++)
        if (rd32(f + phoff + (uint64_t)i * phentsize) == 3) {
            has_interp = 1;
            break;
        } // PT_INTERP
    if (g_nonpie_lo && !has_interp && !go_section_by_name(f, image.size, ".gopclntab", NULL, NULL, NULL)) {
        uint64_t shoff = rd64(f + 40);
        uint16_t shentsize = rd16(f + 58), shnum = rd16(f + 60), shstrndx = rd16(f + 62);
        if (shoff && shnum && shstrndx < shnum) {
            const uint8_t *shstr = f + rd64(f + shoff + (uint64_t)shstrndx * shentsize + 24);
            for (int i = 0; i < shnum; i++) {
                const uint8_t *sh = f + shoff + (uint64_t)i * shentsize;
                const char *nm = (const char *)shstr + rd32(sh + 0);
                if (strcmp(nm, ".data.rel.ro") != 0 && strcmp(nm, ".data") != 0) continue;
                uint64_t saddr = rd64(sh + 16), ssize = rd64(sh + 32);
                for (uint64_t o = 0; o + 8 <= ssize; o += 8) {
                    uint64_t *slot = (uint64_t *)(saddr + bias + o);
                    uint64_t target = *slot;
                    int same_section = target >= saddr && target < saddr + ssize;
                    if (!same_section && target >= g_nonpie_lo && target < g_nonpie_hi) *slot += bias;
                }
            }
        }
    }
    // Per-segment W^X + read-only registry, once the rebases above have finished writing into the image.
    // This used to force the whole span R|W|X while registering the read-only segments anyway, so a store
    // into .rodata found a physically writable page and was silently kept -- elf_protect.h, the contract.
    hl_elf_protect_segments(&map_context.mapping, f + phoff, phnum, phentsize, bias);
    // A dynamic ET_EXEC remains a LOW-address Linux object even though macOS forces its storage HIGH.
    // ld.so derives the main link_map bias and lookup range from AT_ENTRY/AT_PHDR; publishing host-biased
    // values makes dladdr/dlsym miss every LOW function pointer. The dispatcher already translates LOW
    // execution addresses through g_nonpie_bias, matching the AArch64 loader contract.
    out->entry = etype == 2 ? e_entry : e_entry + bias;
    out->base = (uint64_t)base;
    out->phdr = etype == 2 ? basepage + phoff : (uint64_t)base + phoff;
    out->phent = phentsize;
    out->phnum = phnum;
    extern int g_diag;
    if (g_trace || g_diag)
        fprintf(stderr, "[LOADED] %s base=%llx span=%llx end=%llx entry=%llx\n", path, (unsigned long long)base,
                (unsigned long long)span, (unsigned long long)((uint64_t)base + span), (unsigned long long)out->entry);
    hl_linux_image_release(&image);
}

// Build the SysV x86-64 process stack (identical layout to aarch64). Returns rsp.
static char *g_guest_env[] = {"PATH=/usr/bin:/bin", "HOME=/root", "TERM=dumb", "LANG=C", NULL};

// AT_HWCAP on x86-64 is CPUID.1:EDX (arch/x86/include/asm/elf.h ELF_HWCAP; measured 0x178bfbff natively
// here). It was hardcoded 0. glibc hid that -- getauxval(AT_HWCAP) returns its own _dl_hwcap, not the kernel
// word, which is why the `auxval` fixture still read hwcap_nz=1 -- but a /proc/self/auxv reader saw a CPU with
// no features at all.
//
// Not the host's word: the guest never executes on this CPU. The one model is hl_x86_cpuid()'s leaf 1, where
// -- the rule guest/aarch64/cpu.h records -- a bit is set only when BOTH backends implement the whole feature
// exactly, and which the guest reads directly with `cpuid`. Ask it rather than restate it, so the two surfaces
// cannot disagree. AT_HWCAP2 is 0 by the same derivation, not omission: its bits are RING3MWAIT and FSGSBASE,
// and leaf 7 EBX bit 0 is clear in that model. Linux emits it unconditionally, so the entry must exist at 0.
static uint64_t x86_guest_hwcap(void) {
    struct cpu probe = {0}; // hl_x86_cpuid reads RAX/RCX and writes RAX..RDX; nothing else is touched
    probe.r[RAX] = 1;
    hl_x86_cpuid(&probe);
    return (uint64_t)(uint32_t)probe.r[RDX];
}

static uint64_t build_stack(int argc, char **argv, struct loaded *lm, uint64_t at_base) {
    size_t SZ = 8u << 20, GUARD = 0x10000;
    // stack-overflow safety: a PROT_NONE guard gap immediately BELOW the usable stack (Linux's
    // stack_guard_gap, 1MB). Without it the stack sits adjacent-above the 64MB RX code cache and a deep
    // recursion / huge frame overruns straight into the executable cache -> silent corruption (clickhouse)
    // instead of a fault. A store past the bottom now hits PROT_NONE; jit86_lazyguard sees the gna_add'd HARD
    // guard (NOT growable -- so its lazy zero-page grower can't silently swallow the overflow) and delivers
    // SIGSEGV(SEGV_MAPERR), byte-exact with the qemu oracle. The separate top GUARD stays R+W for the SSE
    // over-read past the logical top below.
    size_t LOGUARD = 1u << 20;
    // The top GUARD bytes are mapped ABOVE the logical top: the topmost stack objects are the
    // AT_PLATFORM "x86_64" string and the 16 AT_RANDOM bytes, which glibc strlen/reads
    // with 16-byte SSE loads -> those over-read past the top. Keep that region mapped.
    const hl_host_services *host = effective_host_services();
    hl_host_memory_mapping stack_mapping = {HL_HOST_MEMORY_MAPPING_ABI, sizeof(stack_mapping), 0, 0, 0, 0};
    uint64_t stack_address = hl_option_get("HL_CHECKPOINT") ? UINT64_C(0x0000058000000000) : 0;
    hl_host_result stack_result =
        host->memory->map_anonymous(host->context, stack_address, LOGUARD + SZ + GUARD,
                                    HL_HOST_MEMORY_READ | HL_HOST_MEMORY_WRITE, HL_HOST_MEMORY_PRIVATE, &stack_mapping);
    if (stack_result.status != HL_STATUS_OK) {
        fprintf(stderr, "hl-engine: cannot map x86 guest main stack (host status %d)\n", stack_result.status);
        exit(1);
    }
    uint8_t *base = (uint8_t *)(uintptr_t)stack_mapping.address;
    (void)host->memory->protect(host->context, stack_mapping.handle, 0, LOGUARD, 0);
    gna_add((uint64_t)base, (uint64_t)base + LOGUARD);
    uint8_t *stk = base + LOGUARD;
    if (hl_exec_mapping_add((uint64_t)base, LOGUARD + SZ + GUARD, stack_mapping.handle) != 0) {
        (void)host->memory->release(host->context, stack_mapping.handle);
        fprintf(stderr, "hl-engine: loader mapping registry exhausted\n");
        exit(1);
    }
    hl_gmap_add((uint64_t)stk, SZ + GUARD);
    uint8_t *top = stk + SZ;
    extern uint64_t g_stack_lo, g_stack_hi; // publish for /proc/self/maps [stack] synthesis (vfs.c)
    g_stack_lo = (uint64_t)stk;
    g_stack_hi = (uint64_t)(stk + SZ + GUARD);
    uint64_t argp[HL_MAXARGV], envp_[256]; // argv can be large post-exec (ARG_MAX); env stays small
    set_guest_cmdline(argc, argv);         // capture the full argv for /proc/self/cmdline (bare-mode fallback)
    int envc = 0;
    for (int i = 0; i < argc; i++) {
        size_t l = strlen(argv[i]) + 1;
        top -= l;
        memcpy(top, argv[i], l);
        argp[i] = (uint64_t)top;
    }
    // The container's env arrives as HL_GUEST_ENV="K=V\nK=V\n…" (set by launch config / forwarded across
    // execve by exec_forward_env). Forward EXACTLY those FIRST so they override the built-in defaults; the
    // defaults then fill ONLY the keys the container didn't set (match on the "KEY=" prefix). Mirrors the
    // shared aarch64 build_stack (linux_abi/elf.c) -- without this, x86 guests ignored the container env.
    const char *estr[256];
    const char *ge = hl_option_get("HL_GUEST_ENV");
    char *gecopy = NULL;
    // execve() escape-encodes records (HL_GUEST_ENV_ESC=1) so a value's own newline isn't mistaken for a
    // record separator -- unescape "\\n"->'\n' and "\\\\"->'\\' after splitting. Mirrors linux_abi/elf.c.
    int env_escaped = (hl_option_get("HL_GUEST_ENV_ESC") != NULL);
    // Guest-initiated execve makes its envp authoritative (exec_forward_env sets HL_GUEST_ENV_EXACT): forward
    // it verbatim and inject NO fallback defaults, so NULL/curated envp matches Linux. Mirrors linux_abi/elf.c.
    int env_exact = (hl_option_get("HL_GUEST_ENV_EXACT") != NULL);
    if (ge) {
        gecopy = strdup(ge);
        char *save = NULL;
        for (char *ln = strtok_r(gecopy, "\n", &save); ln && envc < 250; ln = strtok_r(NULL, "\n", &save)) {
            if (env_escaped) {
                char *r = ln, *w = ln; // unescape in place (only ever shrinks)
                while (*r) {
                    if (r[0] == '\\' && r[1] == 'n') {
                        *w++ = '\n';
                        r += 2;
                    } else if (r[0] == '\\' && r[1] == '\\') {
                        *w++ = '\\';
                        r += 2;
                    } else {
                        *w++ = *r++;
                    }
                }
                *w = 0;
            }
            estr[envc++] = ln;
        }
    }
    int guest_envc = envc;
    for (int i = 0; !env_exact && g_guest_env[i] && envc < 255; i++) {
        const char *eq = strchr(g_guest_env[i], '=');
        size_t klen = eq ? (size_t)(eq - g_guest_env[i]) + 1 : 0;
        int dup = 0;
        for (int j = 0; j < guest_envc && klen; j++)
            if (strncmp(estr[j], g_guest_env[i], klen) == 0) {
                dup = 1;
                break;
            }
        if (dup) continue;
        estr[envc++] = g_guest_env[i];
    }
    set_guest_environ(estr, envc); // capture the final env for /proc/self/environ (== getenv)
    for (int i = 0; i < envc; i++) {
        size_t l = strlen(estr[i]) + 1;
        top -= l;
        memcpy(top, estr[i], l);
        envp_[i] = (uint64_t)top;
    }
    free(gecopy); // the HL_GUEST_ENV tokens (estr[..]) were copied onto the stack above; safe to release now
    // AT_EXECFN string: Linux copies the execve PATHNAME (not argv[0]) near the stack top and points
    // AT_EXECFN at it. Rust std / uutils' multicall read it; a relative argv[0] (fork+exec'd `./x` or
    // execve("/proc/self/exe")) diverged from the native absolute path. g_exe_path holds the canonical
    // guest exec path (set by the loader and by execve before this call); fall back to argv[0].
    const char *execfn_str = (g_exe_path && g_exe_path[0]) ? g_exe_path : (argc ? argv[0] : "");
    size_t execfn_len = strlen(execfn_str) + 1;
    top -= execfn_len;
    memcpy(top, execfn_str, execfn_len);
    uint64_t execfn = (uint64_t)top;
    top -= 8;
    memcpy(top, "x86_64", 7);
    uint64_t plat = (uint64_t)top;
    top -= 16;
    arc4random_buf(top, 16);
    uint64_t rnd = (uint64_t)top;
    top = (uint8_t *)((uint64_t)top & ~15ull);
    uint64_t aux[][2] = {
        {3, lm->phdr},
        {4, (uint64_t)lm->phent},
        {5, (uint64_t)lm->phnum},
        {6, HL_LINUX_GUEST_PAGE_SIZE},
        {7, at_base},
        {8, 0},
        {9, lm->entry},
        // AT_UID/EUID/GID/EGID: 0 with a "container root" comment, but cuid()/cgid() ARE the container
        // identity (state.c: configured id, else the host's) and are what container_init seeds g_ruid/g_euid
        // from -- so the constant did not mean root, it meant auxv contradicted this guest's own getuid() and
        // the aarch64 engine. Measured: aarch64 guest 1000, x86-64 guest 0, same uncontainerised run.
        {11, (uint64_t)cuid()},
        {12, (uint64_t)cuid()},
        {13, (uint64_t)cgid()},
        {14, (uint64_t)cgid()},
        {16, x86_guest_hwcap()},
        {15, plat},
        {25, rnd},
        {23, 0},      // AT_SECURE 0
        {17, 100},    // AT_CLKTCK
        {26, 0},      // AT_HWCAP2 -- 0 by derivation, see x86_guest_hwcap()
        {31, execfn}, // AT_EXECFN -> execve pathname string (glibc/Rust/uutils multicall read it). Missing it
                      // made getauxval(AT_EXECFN)==0 -> strlen(0) -> SIGSEGV.
        {0, 0},       // AT_NULL terminator
    };
    int naux = (int)(sizeof aux / sizeof aux[0]);
    size_t nslots = 1 + (argc + 1) + (envc + 1) + (size_t)naux * 2;
    uint64_t *sp = (uint64_t *)top - nslots;
    sp = (uint64_t *)((uint64_t)sp & ~15ull);
    uint64_t *p = sp;
    *p++ = (uint64_t)argc;
    for (int i = 0; i < argc; i++)
        *p++ = argp[i];
    *p++ = 0;
    for (int i = 0; i < envc; i++)
        *p++ = envp_[i];
    *p++ = 0;
    for (int i = 0; i < naux; i++) {
        *p++ = aux[i][0];
        *p++ = aux[i][1];
    }
    // Serialize the same auxv for /proc/self/auxv (read by Rust std / hwcap crates; the x86 path previously
    // left it empty -> a 0-length auxv that those readers mis-parse). g_auxv_data/_len live in vfs.c (same TU).
    g_auxv_len = 0;
    for (int i = 0; i < naux && g_auxv_len + 16 <= (int)sizeof g_auxv_data; i++) {
        memcpy(g_auxv_data + g_auxv_len, &aux[i][0], 8);
        memcpy(g_auxv_data + g_auxv_len + 8, &aux[i][1], 8);
        g_auxv_len += 16;
    }
    extern int g_diag;
    if (g_diag)
        fprintf(stderr, "[stack] base=%p top=%p guard_end=%p sp=%p plat=%llx rnd=%llx\n", (void *)stk, (void *)top,
                (void *)(stk + SZ + GUARD), (void *)sp, (unsigned long long)plat, (unsigned long long)rnd);
    return (uint64_t)sp;
}

// debug fault handler (only installed under TRACE_ON): print faulting address + guest cpu.
// Lazy-guard fault handler (default): glibc's vectorized string ops (strlen/memchr/
// memcmp) issue 16-byte SSE loads that legitimately over-read past a buffer's end into
// the adjacent page. On Darwin an unmapped page -> SIGBUS. We map the faulting page as
// zero and retry: the zero terminator makes strlen/memchr return the correct result, and
// vectorized loads mask out the bytes past the real end. Bounded so genuine wild
// accesses (a real bug) still abort once the budget is spent.
static _Atomic int g_lazymaps; // isolated/wild faults (small bounded budget)
static _Atomic int g_growmaps; // adjacent (stack-grow / over-read) faults: large bounded budget

// W6A item 4: the original cap was a single global, monotonic, never-reset budget of 4096 pages shared
// by BOTH legitimate growth (stack-down, SSE over-reads adjacent to a real allocation) AND genuine wild
// pointers. A long-running / large-working-set guest that legitimately faults >4096 DISTINCT guard pages
// exhausts it and the next legitimate fault is re-raised as a fatal SIGSEGV (exit 139). Fix: classify the
// fault by adjacency to an existing mapping. A fault page whose immediate neighbor (above OR below) is
// already mapped is provably legitimate (stack growth is one page below the committed stack; an SSE
// over-read is one page past a real buffer) -> map it against a large grow budget. A fault with NO mapped
// neighbor is an isolated wild pointer -> the small bounded budget still catches it and aborts (safety
// net PRESERVED). Page contents + retry are unchanged, so this is bit-identical for any workload the old
// code completed. Gate: NOLAZYFIX=1 reverts to the single 4096 monotonic budget (everything on g_lazymaps);
// LAZYBUDGET=<n> overrides the small cap (repro/testing); LAZYDIAG=1 prints final counts at exit.
static int lazy_neighbor_mapped(uintptr_t pg) {
    // A fault adjacent to a live mapping is legitimate growth/over-read: the byte just below the fault
    // page is the end of a real region (over-read), or the page just above is the committed stack
    // (grow-down). An isolated fault (both neighbors unmapped) is a candidate wild pointer. Probe above
    // using the native host VM page size: 16 KiB on the primary macOS host and commonly 4 KiB on Linux.
    return hl_host_page_neighbor_mapped(pg);
}

static int lazy_budget(void) {
    return 4096;
}

static int lazy_nofix(void) {
    return 0;
}

static void lazy_diag(void) {
}

// W6A item 1: emulate a faulting host load/store against the biased non-PIE image. A non-PIE guest's
// absolute ref resolves to the original low link vaddr (in [g_nonpie_lo,g_nonpie_hi)); the real data
// lives at that vaddr + g_nonpie_bias. We decode the faulting emitted arm64 access, perform it at +bias,
// and skip the instruction. Every guest memory access is emitted with the effective address pre-folded
// into x17 (off=0) -> si_addr is the access base. Three families are served:
//   * INTEGER ld/st-register (scaled uimm + unscaled ldur/stur, signed/unsigned, b/h/w/x),
//   * SIMD&FP ld/st-register Q/D/S/H/B (the SSE constant/spill paths emit `ldr q`/`str q` etc. against
//     low .rodata/.data) -- moved through the ucontext NEON state (__ns.__v[t]); FP loads zero the upper
//     lanes, matching arm64,
//   * LSE atomic RMW (ldadd/ldclr/ldeor/ldset/swp) + compare-and-swap (cas) -- the x86 LOCK path emits
//     these against the absolute EA; performed ATOMICALLY at +bias, old value written back to the reg.
// Returns 1 if handled. Anything it can't decode safely (e.g. an LSE signed/unsigned min/max subform the
// x86 backend never emits) returns 0 -> the normal handler re-raises = a clean abort, never silent wrong
// data. Gated on g_nonpie_lo (set only for ET_EXEC) -> PIE/static-PIE never enter here. OUT OF SCOPE
// (documented residual, a separate broad g2h change): syscall POINTER args that point into the low
// non-PIE image are read 1:1 in service.c and are NOT redirected here.

// HOST-CPU GATE to the end of nonpie_fixup: it decodes a 4-byte A64 word at the host PC out of
// HL_HOST_UC_REGS/VREGS, so another host backend needs its own absolute-DATA fixup in the #else arm.
#if defined(HL_HOST_HAS_A64_CONTEXT)

// Atomic RMW helpers (truly atomic, width-typed) used by the LSE/CAS fixup paths below.
static int nonpie_lse_rmw(void *p, int size, int opc, uint64_t v, uint64_t *old) {
    // opc: 0=ADD 1=CLR(&~) 2=EOR 3=SET(|). Returns 1 if handled, 0 for an unsupported subform.
    switch (size) {
#define NP_RMW(TY)                                                                                                     \
    {                                                                                                                  \
        TY *a = (TY *)p, ov = (TY)v, o;                                                                                \
        switch (opc) {                                                                                                 \
        case 0: o = __atomic_fetch_add(a, ov, __ATOMIC_SEQ_CST); break;                                                \
        case 1: o = __atomic_fetch_and(a, (TY)~ov, __ATOMIC_SEQ_CST); break;                                           \
        case 2: o = __atomic_fetch_xor(a, ov, __ATOMIC_SEQ_CST); break;                                                \
        case 3: o = __atomic_fetch_or(a, ov, __ATOMIC_SEQ_CST); break;                                                 \
        default: return 0;                                                                                             \
        }                                                                                                              \
        *old = (uint64_t)o;                                                                                            \
        return 1;                                                                                                      \
    }
    case 0: NP_RMW(uint8_t)
    case 1: NP_RMW(uint16_t)
    case 2: NP_RMW(uint32_t)
    default: NP_RMW(uint64_t)
#undef NP_RMW
    }
}

static uint64_t nonpie_lse_swp(void *p, int size, uint64_t v) {
    switch (size) {
    case 0: return __atomic_exchange_n((uint8_t *)p, (uint8_t)v, __ATOMIC_SEQ_CST);
    case 1: return __atomic_exchange_n((uint16_t *)p, (uint16_t)v, __ATOMIC_SEQ_CST);
    case 2: return __atomic_exchange_n((uint32_t *)p, (uint32_t)v, __ATOMIC_SEQ_CST);
    default: return __atomic_exchange_n((uint64_t *)p, v, __ATOMIC_SEQ_CST);
    }
}

static uint64_t nonpie_cas(void *p, int size, uint64_t expected, uint64_t newv) {
    // Compare-and-swap; returns the pre-CAS memory value. __atomic_compare_exchange_n leaves the loaded
    // value in `e` on failure, and `e` unchanged (== old, since it matched) on success -> `e` is the old
    // value in both cases, which is what cas writes back into Rs.
    switch (size) {
    case 0: {
        uint8_t e = (uint8_t)expected;
        __atomic_compare_exchange_n((uint8_t *)p, &e, (uint8_t)newv, 0, __ATOMIC_SEQ_CST, __ATOMIC_SEQ_CST);
        return e;
    }
    case 1: {
        uint16_t e = (uint16_t)expected;
        __atomic_compare_exchange_n((uint16_t *)p, &e, (uint16_t)newv, 0, __ATOMIC_SEQ_CST, __ATOMIC_SEQ_CST);
        return e;
    }
    case 2: {
        uint32_t e = (uint32_t)expected;
        __atomic_compare_exchange_n((uint32_t *)p, &e, (uint32_t)newv, 0, __ATOMIC_SEQ_CST, __ATOMIC_SEQ_CST);
        return e;
    }
    default: {
        uint64_t e = expected;
        __atomic_compare_exchange_n((uint64_t *)p, &e, newv, 0, __ATOMIC_SEQ_CST, __ATOMIC_SEQ_CST);
        return e;
    }
    }
}

// zero-extend a `size`-byte value to register width (matches W-register upper-32 clearing for size<8).
static uint64_t nonpie_zext(uint64_t v, int size) {
    return size >= 3 ? v : (v & ((1ull << (8 << size)) - 1));
}

static int nonpie_fixup(siginfo_t *si, void *ucv) {
    if (!g_nonpie_lo || !ucv || !si) return 0;
    uint64_t va = (uint64_t)si->si_addr;
    if (va < g_nonpie_lo || va >= g_nonpie_hi) return 0;
    ucontext_t *uc = (ucontext_t *)ucv;
    uint32_t insn = *(uint32_t *)(HL_HOST_UC_PC(uc));
    uint64_t real = va + g_nonpie_bias; // the actual mapped location of the datum
    uint64_t *X = HL_HOST_UC_REGS(uc);  // x[0..30]
    __uint128_t *V = HL_HOST_UC_VREGS(uc);
    int v = (insn >> 26) & 1; // SIMD&FP?
    int rt = insn & 0x1F;
    // Load/store-register IMMEDIATE family: bits[29:27]==111, and either the scaled unsigned-imm form
    // (bit24==1) or the unscaled ldur/stur form (bit24==0 && bit21==0 && bits[11:10]==00). This EXCLUDES
    // the register-offset form (bit21==1) and the LSE atomics (bit21==1, decoded separately below) -- both
    // also have bits[29:27]==111, but the backend never emits a register-offset access for a guest EA.
    int fam = ((insn >> 27) & 7) == 7;
    int scaled = (insn >> 24) & 1;
    int ls_imm = fam && (scaled || (!((insn >> 21) & 1) && !((insn >> 10) & 3)));

    // ---- SIMD&FP Q/D/S/H/B load/store (V==1). width = 1<<((opc[1]<<2)|size): B1 H2 S4 D8 Q16. ----
    if (v && ls_imm) {
        if (V == NULL) return 0;
        int size = insn >> 30, opc = (insn >> 22) & 3;
        int bytes = 1 << (((opc >> 1) << 2) | size);
        if (opc & 1) { // load -> write Vt, zeroing the upper lanes (arm64 FP-load semantics)
            __uint128_t z = 0;
            memcpy(&z, (void *)real, (size_t)bytes);
            V[rt] = z;
        } else { // store -> low `bytes` of Vt to memory
            __uint128_t s = V[rt];
            memcpy((void *)real, &s, (size_t)bytes);
        }
        HL_HOST_UC_PC(uc) += 4;
        return 1;
    }

    // ---- LDAPR (x86-TSO acquire load, emit.c e_ldapr) -- must precede the LSE decode below, whose mask
    // LDAPR also matches (it lands on o3=1/opc=4 there and would decline). Stack accesses (push/pop/call/
    // ret) use the guest RSP register directly and skip the address emitter's runtime bias fold, so a
    // non-PIE guest running on a stack inside its own low image (makecontext/coroutine stacks in .bss)
    // arrives here: the store half is already served by the STR/STUR path, the load half is this.
    if ((insn & 0x3FFFFC00u) == 0x38BFC000u) {
        int size = insn >> 30; // 0=B 1=H 2=W 3=X
        uint64_t val = 0;
        memcpy(&val, (const void *)real, (size_t)1u << size); // little-endian host==guest; zero-extends
        __asm__ __volatile__("dmb ishld" ::: "memory");       // the acquire edge LDAPR would have supplied
        if (rt != 31) X[rt] = val;
        HL_HOST_UC_PC(uc) += 4;
        return 1;
    }

    // ---- LSE atomic RMW: size[31:30] 111 0 00 A R 1 Rs[20:16] o3[15] opc[14:12] 00 Rn[9:5] Rt[4:0] ----
    if ((insn & 0x3F200C00u) == 0x38200000u) {
        int size = insn >> 30, o3 = (insn >> 15) & 1, opc = (insn >> 12) & 7;
        int rs = (insn >> 16) & 0x1F;
        uint64_t operand = (rs == 31) ? 0 : X[rs], old;
        if (o3 && opc == 0) { // swp: x = [m]; [m] = operand
            old = nonpie_lse_swp((void *)real, size, operand);
        } else if (!o3 && opc < 4) { // ldadd / ldclr / ldeor / ldset
            if (!nonpie_lse_rmw((void *)real, size, opc, operand, &old)) return 0;
        } else {
            return 0; // signed/unsigned min/max (never emitted by the x86 backend) -> clean abort
        }
        if (rt != 31) X[rt] = nonpie_zext(old, size); // Rt receives the old value
        HL_HOST_UC_PC(uc) += 4;
        return 1;
    }

    // ---- CAS/CASAL: size[31:30] 001000 1 1 1 Rs[20:16] o0 11111 Rn[9:5] Rt[4:0]. Rs=cmp in/old out. ----
    if ((insn & 0x3FE0FC00u) == 0x08E0FC00u) {
        int size = insn >> 30, rs = (insn >> 16) & 0x1F;
        uint64_t expected = (rs == 31) ? 0 : X[rs], newv = (rt == 31) ? 0 : X[rt];
        uint64_t old = nonpie_cas((void *)real, size, expected, newv);
        if (rs != 31) X[rs] = nonpie_zext(old, size); // Rs receives the old value
        HL_HOST_UC_PC(uc) += 4;
        return 1;
    }

    // ---- INTEGER load/store-register (scaled + unscaled, signed/unsigned, b/h/w/x) ----
    if (!(ls_imm && !v)) return 0; // not a form we decode -> clean abort (the handler re-raises)
    int size = insn >> 30;         // 0=B 1=H 2=W 3=X
    int opc = (insn >> 22) & 3;    // 01=load(zext) 00=store 10=load-sext(64) 11=ldrsw
    uint64_t val;
    if (opc == 0) { // store: write rt's low `size` bytes
        val = (rt == 31) ? 0 : X[rt];
        switch (size) {
        case 0: *(uint8_t *)real = (uint8_t)val; break;
        case 1: *(uint16_t *)real = (uint16_t)val; break;
        case 2: *(uint32_t *)real = (uint32_t)val; break;
        default: *(uint64_t *)real = val; break;
        }
    } else { // load
        switch (size) {
        case 0:
            val = *(uint8_t *)real;
            if (opc == 2) val = (uint64_t)(int64_t)(int8_t)val;
            break;
        case 1:
            val = *(uint16_t *)real;
            if (opc == 2) val = (uint64_t)(int64_t)(int16_t)val;
            break;
        case 2:
            val = *(uint32_t *)real;
            if (opc == 2 || opc == 3) val = (uint64_t)(int64_t)(int32_t)val;
            break;
        default: val = *(uint64_t *)real; break;
        }
        if (rt != 31) X[rt] = val;
    }
    HL_HOST_UC_PC(uc) += 4; // skip the faulting load/store
    return 1;
}

#else

// Not an AArch64 host: nothing to decode. 0 ("not handled") keeps the fault on the deliver/re-raise path.
static int nonpie_fixup(siginfo_t *si, void *ucv) {
    (void)si;
    (void)ucv;
    return 0;
}

#endif

// Unaligned guest ATOMIC (LSE) alignment-fault fixup.
//
// x86 makes `xchg reg,mem` implicitly locked and lets `lock` prefix any of add/adc/sub/sbb/and/or/xor/
// btc/bts/btr/xadd/cmpxchg -- at ANY address, aligned or not. A split-lock access is legal x86 and real
// hardware honours it (it locks the bus). The backend lowers all of those to single LSE instructions
// (SWPAL / LDADDAL / LDCLRAL / LDEORAL / LDSETAL / CASAL, emit.c e_lse / e_cas), and an LSE atomic to an
// address that is not naturally aligned raises SIGBUS/BUS_ADRALN on AArch64 (an unaligned atomic is
// CONSTRAINED UNPREDICTABLE-free only when SCTLR.nAA permits it, and never across a 16-byte granule).
// Without this fixup an otherwise-valid guest program dies of SIGBUS.
//
// Exactly as for the LDAPR fixup above: a BUS_ADRALN raised at an engine-emitted LSE atomic host PC is
// ALWAYS synthetic (x86 permits the access), never a guest-visible fault, so emulating it and resuming is
// sound. The emulation performs the read-modify-write under a dedicated global lock with acquire-release
// barriers, which keeps it atomic against OTHER unaligned guest atomics -- the case a split-lock guest
// actually exercises. It is NOT mutually exclusive with the aligned LSE fast path on an overlapping
// address; making that work would require stopping the world on every atomic (what qemu-user does via
// cpu_loop_exit_atomic) and would tax every correctly-aligned lock in every guest. The trade is
// deliberate and documented: correctness for split-lock programs, no cost on the aligned path.

// HOST-CPU GATE, as for nonpie_fixup: this decodes A64 LSE/CASAL, and only AArch64 faults on an unaligned
// atomic at all (x86's `lock`/`xchg` are legal at any address), so on x86-64 that SIGBUS cannot arise.
#if defined(HL_HOST_HAS_A64_CONTEXT)

static _Atomic unsigned g_unaligned_atomic_lock;

static int lse_align_fixup(int sig, siginfo_t *si, void *ucv) {
    if (sig != SIGBUS || !si || !ucv) return 0;
#ifdef BUS_ADRALN
    if (si->si_code != BUS_ADRALN) return 0;
#else
    if (si->si_code != 1) return 0;
#endif
    ucontext_t *uc = (ucontext_t *)ucv;
    uint64_t hpc = (uint64_t)HL_HOST_UC_PC(uc);
    extern int jit_pc_in_cache(uint64_t pc, uint64_t *base);
    if (!jit_pc_in_cache(hpc, NULL)) return 0; // not code we emitted
    uint32_t insn = *(uint32_t *)hpc;
    int size = insn >> 30; // 0=B 1=H 2=W 3=X
    unsigned width = 1u << size;
    int rs = (insn >> 16) & 0x1F, rn = (insn >> 5) & 0x1F, rt = insn & 0x1F;
    // Two encodings, and only the two the backend emits:
    //   atomic memory op  op[29:24]=111000 A=1 R=1 bit21=1 bits[11:10]=00, o3[15]/opc[14:12] select the op
    //   CASAL             op[29:21]=001010001 (A=1,L=1), o0[15]=1, Rt2[14:10]=11111
    int is_amo = (insn & 0x3FE00C00u) == 0x38E00000u; // 111000 A=1 R=1 bit21=1 bits[11:10]=00
    int is_cas = (insn & 0x3FE0FC00u) == 0x08E0FC00u;
    unsigned o3opc = (insn >> 12) & 0xF; // [o3, opc]
    if (is_amo) {
        // SWP is o3=1,opc=000 (0b1000); LDADD/LDCLR/LDEOR/LDSET are o3=0,opc=0..3. Nothing else is emitted
        // for a guest atomic, so decline anything else rather than guess at its semantics.
        if (o3opc > 3 && o3opc != 8) return 0;
    } else if (!is_cas) {
        return 0;
    }
    uint64_t *X = HL_HOST_UC_REGS(uc);
    uint64_t addr = (rn == 31) ? (uint64_t)HL_HOST_UC_SP(uc) : X[rn];
    // Only emulate a fully mapped access; otherwise decline so the genuine guest fault is delivered.
    if (!hl_host_range_mapped((uintptr_t)addr, width)) return 0;
    uint64_t sval = (rs == 31) ? 0 : X[rs];
    uint64_t tval = (rt == 31) ? 0 : X[rt];
    if (width < 8) { // the W-form operates on 32 bits; B/H forms on their low bytes
        uint64_t m = (width == 4) ? 0xFFFFFFFFull : ((1ull << (8 * width)) - 1);
        sval &= m;
        tval &= m;
    }
    uint64_t old = 0, neu;
    while (atomic_exchange_explicit(&g_unaligned_atomic_lock, 1u, memory_order_acquire))
        ;
    memcpy(&old, (const void *)addr, width); // little-endian host == little-endian guest
    if (is_cas) {
        neu = (old == sval) ? tval : old; // CASAL: Rs is the comparand, Rt the new value
    } else {
        switch (o3opc) {
        case 0: neu = old + sval; break;  // LDADD
        case 1: neu = old & ~sval; break; // LDCLR
        case 2: neu = old ^ sval; break;  // LDEOR
        case 3: neu = old | sval; break;  // LDSET
        default: neu = sval; break;       // SWP
        }
    }
    memcpy((void *)addr, &neu, width);
    atomic_store_explicit(&g_unaligned_atomic_lock, 0u, memory_order_release);
    // CASAL returns the pre-image in Rs; the LD*/SWP forms return it in Rt. Both zero-extend to 64 bits.
    int dst = is_cas ? rs : rt;
    if (dst != 31) X[dst] = old;
    HL_HOST_UC_PC(uc) += 4;
    return 1;
}

#else

// Not an AArch64 host: no LSE atomic was emitted. 0 ("not handled") lets a real SIGBUS reach the crash path.
static int lse_align_fixup(int sig, siginfo_t *si, void *ucv) {
    (void)sig;
    (void)si;
    (void)ucv;
    return 0;
}

#endif

// x86-TSO LDAPR alignment-fault fixup. Guest loads are emitted as LDAPR (Load-AcquirePC) to supply the
// x86-TSO LoadLoad+LoadStore ordering in one instruction (emit.c). On a FEAT_LSE2 host an unaligned LDAPR
// that crosses a 16-byte granule raises SIGBUS/BUS_ADRALN. x86 permits every unaligned normal load, and a
// guest load is never emitted as any OTHER alignment-checked host instruction (plain LDR does not
// alignment-fault on Normal cacheable memory) -- therefore a BUS_ADRALN at an engine-emitted LDAPR host PC
// is ALWAYS this synthetic case, NEVER a guest-visible fault. Emulate the load with a plain unaligned read
// plus DMB ISHLD (the exact LoadLoad+LoadStore acquire edges LDAPR provides -- identical to the old
// LDR+DMB ISHLD sequence), write the zero-extended value into Rt, and step the host PC past the LDAPR.
// Returns 1 iff handled; declines (0) for anything that is not one of our LDAPRs so real faults flow on.

// HOST-CPU GATE, third of the same kind: LDAPR is A64, and x86-64 (TSO gives the acquire edge free) emits
// none and never alignment-faults a load.
#if defined(HL_HOST_HAS_A64_CONTEXT)

static int ldapr_align_fixup(int sig, siginfo_t *si, void *ucv) {
    extern int g_host_lrcpc; // set from host AT_HWCAP (emit.c); 0 => no LDAPR emitted
    if (!g_host_lrcpc || sig != SIGBUS || !si || !ucv) return 0; // inert on the LDR+DMB fallback path
#ifdef BUS_ADRALN
    if (si->si_code != BUS_ADRALN) return 0;
#else
    if (si->si_code != 1) return 0;
#endif
    ucontext_t *uc = (ucontext_t *)ucv;
    uint64_t hpc = (uint64_t)HL_HOST_UC_PC(uc);
    // The faulting instruction must live inside the live RX code arena, else it is not one we emitted.
    extern int jit_pc_in_cache(uint64_t pc, uint64_t *base);
    if (!jit_pc_in_cache(hpc, NULL)) return 0;
    uint32_t insn = *(uint32_t *)hpc;
    // LDAPR{B,H,,} <Rt>,[<Xn>]: mask out size[31:30], Rn[9:5], Rt[4:0].
    if ((insn & 0x3FFFFC00u) != 0x38BFC000u) return 0;
    uint64_t *X = HL_HOST_UC_REGS(uc);
    int size = insn >> 30; // 0=B 1=H 2=W 3=X -> 1/2/4/8 bytes
    int rn = (insn >> 5) & 0x1F;
    int rt = insn & 0x1F;
    uint64_t addr = (rn == 31) ? (uint64_t)HL_HOST_UC_SP(uc) : X[rn];
    unsigned width = 1u << size;
    // A crossing access could span into an unmapped page (a genuine guest #PF on x86). Only emulate when
    // the whole access is mapped; otherwise decline so the normal fault path delivers the real fault.
    if (!hl_host_range_mapped((uintptr_t)addr, width)) return 0;
    uint64_t val = 0;
    memcpy(&val, (const void *)addr, width);        // little-endian host==guest; zero-extends to 64 bits
    __asm__ __volatile__("dmb ishld" ::: "memory"); // acquire: order the load before later loads/stores
    if (rt != 31) X[rt] = val;                      // Rt==31 would be ZR (discard); never emitted for a load
    HL_HOST_UC_PC(uc) += 4;
    return 1;
}

#else

// Not an AArch64 host: no LDAPR was emitted. 0 ("not handled") lets a genuine alignment fault be reported.
static int ldapr_align_fixup(int sig, siginfo_t *si, void *ucv) {
    (void)sig;
    (void)si;
    (void)ucv;
    return 0;
}

#endif

void jit86_lazyguard(int sig, siginfo_t *si, void *uc) {
    // x86-TSO LDAPR unaligned-crossing alignment fault -> emulate + resume. First, before any classifier:
    // this synthetic BUS_ADRALN is neither a guest fault nor a lazy-map candidate. (No-op unless the fault
    // is a BUS_ADRALN at an engine LDAPR host PC.)
    if (ldapr_align_fixup(sig, si, uc)) return;
    // Same shape, for guest ATOMICs: an unaligned (split-lock) x86 lock/xchg lowered to an LSE atomic
    // alignment-faults on AArch64 although x86 permits it. Emulate + resume; see lse_align_fixup.
    if (lse_align_fixup(sig, si, uc)) return;
    // host_range_mapped's fault-guarded probe (thread.c): a probe load on an unmapped guest page long-jumps
    // back to report "unmapped" -> -EFAULT. MUST run first: the lazy zero-page mapper below would otherwise
    // serve the probe fault with a fresh mapping, flipping a correct EFAULT into a bogus success (and
    // burning lazy-map budget); nonpie_fixup would likewise emulate the probe load at +bias and resume.
    if (hrm_fault_hook(si)) return; // never actually returns on a claim (siglongjmp); shape-only
#if defined(__linux__)
    /* Linux distinguishes an externally sent fault-class signal from a hardware fault in si_code.
     * Route it before the lazy-map classifier examines si_addr: SI_USER/SI_TKILL/SI_QUEUE may carry an
     * address-shaped union value, but they must interrupt pause/read/etc., never allocate a guest page. */
    if (si && si->si_code <= 0 && deliver_guest_fault(sig, si, uc)) return;
#endif
    // a bad guest RESULT pointer in the vDSO fast-clock inline path (emit_fast_syscall). Recover
    // it as -EFAULT BEFORE the lazy-map/guest-fault paths below, matching the slow svc_time() path exactly.
    if (fastclk_fault_fixup(si, uc)) return;
#if defined(__linux__)
    // A hardware-raised host SIGBUS (si_code > 0: BUS_ADRERR past-EOF file mapping, BUS_ADRALN misalignment)
    // is a GENUINE guest bus error on a Linux host, where the kernel raises SIGBUS authoritatively. It must
    // reach the guest's SIGBUS handler (BUS_ADRERR) or terminate the guest by SIGBUS, and must NEVER fall
    // into the lazy zero-page grower below: that page sits ADJACENT to the file's mapped first page, so
    // lazy_neighbor_mapped() judges it "legitimate growth", skips deliver_guest_fault, and maps an anonymous
    // zero page over it -- silently turning a bus error into a bogus zero read (and, for a handler-armed
    // guest, crashing the engine's own fault path). Route it straight to the guest, mirroring the gna/gro
    // hard-fault blocks below and the dispatcher's raise_guest_bus ledger path.
    if (sig == SIGBUS && si && si->si_code > 0) {
        if (deliver_guest_fault(sig, si, uc)) return;       // guest handler
        if (deliver_guest_fatal_fault(sig, si, uc)) return; // no handler -> faithful WIFSIGNALED SIGBUS
        signal(sig, SIG_DFL);
        raise(sig);
        return;
    }
#endif
    // W6A item 1: a non-PIE absolute DATA ref into the low link range -> serve the access at +bias and
    // advance the host PC. Inert unless g_nonpie_lo is set (ET_EXEC only).
    if (nonpie_fixup(si, uc)) return;
    // a fault inside a tracked guest PROT_NONE region -- hl's main-stack guard gap OR a page the guest
    // itself made PROT_NONE (glibc thread-stack guard, malloc-arena guard, an mmap(PROT_NONE) reservation) --
    // is a HARD fault. Deliver SIGSEGV to the guest (or, with no handler, die of it) and NEVER fall into the
    // lazy zero-page grower below: it would see the mapped stack/heap neighbor, mprotect the guard R+W, and
    // silently swallow a stack overflow into the executable code cache (the clickhouse corruption class).
    // Matches Linux: a write to a PROT_NONE page always faults. Run before smc/lazy; hrm/fastclk (syscall
    // probes, above) still win first so a bad guest buffer into the guard returns -EFAULT as before.
    if (si && si->si_addr && gna_hit((uint64_t)si->si_addr, 1)) {
        if (deliver_guest_fault(sig, si, uc)) return;       // guest handler
        if (deliver_guest_fatal_fault(sig, si, uc)) return; // no handler -> faithful WIFSIGNALED termination
        signal(sig, SIG_DFL);
        raise(sig);
        return;
    }
    // A store into a guest read-only mapping physically faults on the host. It must be delivered to the
    // guest, never consumed by the lazy mapper below (which would mprotect the page RW and retry it).
    // Host reads remain legal under PROT_READ, so any protection fault in this registry is a write fault.
    if (si && si->si_addr && gro_hit((uint64_t)si->si_addr, 1)) {
        if (deliver_guest_fault(sig, si, uc)) return;
        if (deliver_guest_fatal_fault(sig, si, uc)) return;
        signal(sig, SIG_DFL);
        raise(sig);
        return;
    }
    // W6A item 3 (SMC): a guest write to a translated, write-protected JIT code page. Drop the cached
    // translations + IBTC (they're stale; do NOT reset g_cp -> the currently-running block's host code
    // stays intact, orphaned translations are reclaimed by the normal wholesale flush), unprotect the
    // page (smc_on_write retries + the write lands), and let the modified bytes re-translate on next
    // execution. smc_on_write is inert unless a JIT guest is present (g_rwx_guest) -> matrix bit-exact.
    if (si && si->si_addr && smc_on_write((uint64_t)si->si_addr)) {
        map_clear();
        memset(g_ibtc, 0, sizeof g_ibtc);
        // (PyPy JIT bridge coherence): the x86 opt2 2-way IBTC (g_xibtc) is read by the hot
        // indirect-branch fast path and keyed by guest PC -> host body. It MUST be dropped here too, or a
        // surviving entry re-dispatches the just-patched code (e.g. a PyPy guard whose rel32 was rewritten
        // to point at a freshly-assembled bridge) to its STALE pre-patch host body -> the old jump target
        // still fires -> PyPy re-bridges the same guard -> `assert adr_jump_offset != 0` (assemble_bridge/
        // patch_jump_for_descr) fatals. The wholesale flush drops it via G_SHADOW_CLEAR; mirror that here.
        memset(g_xibtc, 0, sizeof g_xibtc);
        pend_reset();
        return;
    }
    // A genuine guest fault (isolated wild pointer / null deref) with a registered handler is the guest's
    // to handle; legitimate glibc vector over-reads are ADJACENT to a live mapping and still fall through
    // to the lazy zero-page map below.
    {
        void *fa = si ? si->si_addr : NULL;
        uintptr_t fpg = (uintptr_t)fa & ~(uintptr_t)0xFFF;
        if (!(fa && !lazy_nofix() && lazy_neighbor_mapped(fpg)) && deliver_guest_fault(sig, si, uc)) return;
    }
    void *a = si ? si->si_addr : NULL;
    if (a) {
        uintptr_t pg = (uintptr_t)a & ~(uintptr_t)0xFFF;
        // W6A item 4: classify by adjacency. A fault adjacent to an existing mapping is legitimate
        // growth/over-read and draws on the large grow budget; an isolated fault is a candidate wild
        // pointer on the small budget. NOLAZYFIX=1 forces the legacy single small monotonic budget.
        int adjacent = !lazy_nofix() && lazy_neighbor_mapped(pg);
#if defined(__linux__)
        // On a Linux host an ISOLATED fault (no mapped neighbor) is a genuine wild pointer: the kernel raises
        // it exactly like hardware, and the aarch64 guest path (which has no lazy grower at all) already
        // faults on it. Silently satisfying it with a zero page is an isolation/correctness hole -- a guest
        // could read unmapped high-VA memory and see 0 instead of SIGSEGV(SEGV_MAPERR). Keep only the ADJACENT
        // grow cushion (a legitimate stack-grow / SSE over-read one page past a live mapping); let an isolated
        // fault fall through to a faithful guest SIGSEGV. The Darwin-only zero-map-and-retry crutch (for
        // over-reads that raise host SIGBUS on macOS) still applies on the non-Linux build below.
        int ok = adjacent && (g_growmaps < (256 << 10)); /* 1GB of grow pages */
#else
        int ok = adjacent ? (g_growmaps < (256 << 10)) /* 1GB of grow pages */ : (g_lazymaps < lazy_budget());
#endif
        if (ok) {
            static int hooked;
            if (!hooked) {
                hooked = 1;
                atexit(lazy_diag);
            }
            // This executes inside a synchronous signal handler. Use only the host service's explicitly
            // signal-context-safe, non-owning exact-page repair; ordinary mapping services take registry locks.
            /* The accessor only reads immutable process-global pointers. Cache it so the signal path
             * performs one accessor read followed by one explicitly signal-context-safe provider call. */
            const hl_host_services *host = effective_host_services();
            if (host->memory->repair_signal_page(host->context, pg, UINT64_C(4096),
                                                 HL_HOST_MEMORY_READ | HL_HOST_MEMORY_WRITE)) {
                if (adjacent)
                    g_growmaps++;
                else
                    g_lazymaps++;
                return;
            }
        }
    }
    // a genuine in-translated-code guest fault with no handler and no legitimate lazy mapping ->
    // terminate the guest process faithfully (WIFSIGNALED/WTERMSIG=sig for its parent) instead of a raw host
    // raise() that degrades to exit(255) across hl's fork. Declines for an engine fault -> real crash below.
    if (deliver_guest_fatal_fault(sig, si, uc)) return;
    signal(sig, SIG_DFL);
    raise(sig); // out of budget / mmap failed -> real crash
}

// Synchronous CPU faults other than SIGSEGV/SIGBUS (which the run path wires to jit86_lazyguard above): a
// guest may install a handler for SIGILL/SIGFPE/SIGTRAP and DELIBERATELY trigger it -- e.g. a CPU-feature
// probe that executes an optional instruction guarded by a SIGILL handler (ud2 / 0F 0B once the feature is
// declared absent), an integer div-by-zero relying on a SIGFPE handler, or an int3 caught via SIGTRAP. The
// x86 frontend emits/raises these as real host signals, but rt_sigaction only records the guest handler --
// it does not install a host handler for synchronous signals (they are served by the guards installed here)
// -- so without this the trap is fatal (exit 255) instead of reaching the guest's handler.
//
// This is the analogue of linux_abi/elf.c's install_sync_fault_guards() (aarch64). We do NOT reuse
// jit86_lazyguard: its lazy zero-page path keys off si_addr, which for these signals is the faulting PC (in
// a mapped, executable JIT page) -- lazy_neighbor_mapped() would judge it "legitimate growth", skip
// deliver_guest_fault, and mprotect/retry the PC page in a loop. Instead route straight to nonpie_fixup
// (which self-declines: si_addr is the high faulting PC, never in the low non-PIE link range) and then
// deliver_guest_fault (delivers the guest signal when the guest has a handler, else re-raises the default).
// CRASHDBG handles these via its mach exception port + diagnostics instead, so leave that path untouched.
static void jit86_syncguard(int sig, siginfo_t *si, void *uc) {
    if (nonpie_fixup(si, uc)) return;
    if (deliver_guest_fault(sig, si, uc)) return;
    signal(sig, SIG_DFL);
    raise(sig);
}

static void jit86_install_sync_fault_guards(void) {
    struct sigaction sa;
    memset(&sa, 0, sizeof sa);
    sa.sa_sigaction = jit86_syncguard;
    sa.sa_flags = SA_SIGINFO;
    sigaction(SIGILL, &sa, NULL);
    sigaction(SIGFPE, &sa, NULL);
    sigaction(SIGTRAP, &sa, NULL);
}

#ifndef HL_EMBEDDED_BUILD
__attribute__((constructor)) static void jit86_sync_fault_guards_constructor(void) {
    jit86_install_sync_fault_guards();
}
#endif

#if defined(_WIN32)
#include "../host/windows/fault.h"

/*
 * The single classifier the host's vectored exception handler calls, standing in for all five sigactions
 * the POSIX arms install.
 *
 * It does no classification of its own. The two guards above already are the classifier -- 200 lines of
 * ordering that decides between a probe fault, an LDAPR/LSE alignment emulation, a non-PIE absolute-data
 * fixup, a PROT_NONE registry hit, a read-only registry hit, self-modifying code, a lazy page grow, guest
 * signal delivery, and a faithful guest termination -- and that ordering is not host-specific. Reproducing
 * it against the host fault record would create a second copy that must stay in step with the first, and
 * the two would diverge on exactly the rare path neither is tested on.
 *
 * So the record is translated INTO the shape the guards already read. That translation is nearly free
 * because the host primitive reports kind and code as the Linux signal number and si_code for the pair; the
 * only field it carries that no POSIX siginfo_t has is `access`, which says whether the instruction read,
 * wrote or executed. si_addr says where, never whether, so sites that today infer write-ness ("a read would
 * have been legal under this protection, so it must have been a write") could stop inferring here. They are
 * left inferring for now: the inference is correct for both registries that use it, and changing it is a
 * behaviour change to a shared path rather than a port.
 *
 * Two things this deliberately does NOT do:
 *
 *   - it never returns DECLINE after calling a guard. A guard that cannot serve the fault does not return:
 *     it terminates the guest faithfully, or restores SIG_DFL and re-raises, which on this host aborts the
 *     process. Returning RESUME after a guard returned therefore always means the guard fixed something.
 *     Returning RESUME on a fault nobody fixed would re-execute the faulting instruction forever, and a
 *     spin is a far worse failure than a crash.
 *   - it takes no lock, allocates nothing and logs nothing on the fast path, because a vectored handler is
 *     entered synchronously from the faulting instruction and may be inside a lock this thread already
 *     holds. The guards obey the same rule; the lazy-map path calls only the host service's explicitly
 *     signal-context-safe page repair for that reason.
 */
static int hl_windows_guest_fault(hl_windows_fault *fault, void *context) {
    siginfo_t info;
    (void)context;
    memset(&info, 0, sizeof info);
    info.si_signo = (int)fault->kind;
    info.si_code = (int)fault->code;
    info.si_addr = (fault->flags & HL_WINDOWS_FAULT_HAS_ADDRESS) ? (void *)(uintptr_t)fault->address : NULL;
    switch (fault->kind) {
    case HL_WINDOWS_FAULT_SEGV:
    case HL_WINDOWS_FAULT_BUS: jit86_lazyguard((int)fault->kind, &info, fault->context); return HL_WINDOWS_FAULT_RESUME;
    case HL_WINDOWS_FAULT_ILL:
    case HL_WINDOWS_FAULT_FPE:
    case HL_WINDOWS_FAULT_TRAP:
        jit86_syncguard((int)fault->kind, &info, fault->context);
        return HL_WINDOWS_FAULT_RESUME;
    default:
        /* Not a class any guard models -- a C++ throw, a debugger breakpoint the
         * debugger owns, a language exception from a loaded DLL. Declining lets
         * the frame-based handlers that DO own it run, which is the whole reason
         * a vectored handler has a decline verdict at all. */
        return HL_WINDOWS_FAULT_DECLINE;
    }
}
#endif

void jit86_faulth(int sig, siginfo_t *si, void *uc) {
    // host_range_mapped probe fault (thread.c) -- resolve it silently even on this diagnostic path, so a
    // FAULT_ON trace run doesn't dump a bogus [FAULT] for every EFAULT-probing syscall and die.
    if (hrm_fault_hook(si)) return; // never actually returns on a claim (siglongjmp); shape-only
    // a non-PIE absolute DATA ref into the low link range is a LEGITIMATE access served at +bias, not a
    // crash -- consult nonpie_fixup FIRST (as the run-path jit86_lazyguard / jit86_syncguard do) so a FAULT_ON
    // diagnostic run of a non-PIE glibc binary (e.g. node --version) resolves and continues instead of dumping
    // a bogus [FAULT] and _exit(133)ing. Self-declines (returns 0) for any address outside [lo,hi) or a host
    // form it can't decode, falling through to the real diagnostics below. Inert for PIE (g_nonpie_lo == 0).
    if (nonpie_fixup(si, uc)) return;
    struct cpu *c = (struct cpu *)pthread_getspecific(g_cpu_key);
    extern uint64_t g_prevpc, g_curpc;
    int diagnostic = open("/tmp/hl-engine-fault.log", O_WRONLY | O_CREAT | O_APPEND, 0600);
    if (diagnostic >= 0) {
        dprintf(diagnostic, "pid=%d sig=%d addr=%p rip=%#llx curpc=%#llx prevpc=%#llx\n", (int)getpid(), sig,
                si ? si->si_addr : 0, (unsigned long long)(c ? c->rip : 0), (unsigned long long)g_curpc,
                (unsigned long long)g_prevpc);
        close(diagnostic);
    }
    static const char *nm[16] = {"rax", "rcx", "rdx", "rbx", "rsp", "rbp", "rsi", "rdi",
                                 "r8",  "r9",  "r10", "r11", "r12", "r13", "r14", "r15"};
    fprintf(stderr, "[FAULT] sig=%d addr=%p  guest rip(last blk)=%llx  curpc=%llx prevblk=%llx ibranch_src=%llx\n", sig,
            si ? si->si_addr : 0, c ? (unsigned long long)c->rip : 0, (unsigned long long)g_curpc,
            (unsigned long long)g_prevpc, c ? (unsigned long long)c->dbg_ibsrc : 0);
    if (c)
        for (int i = 0; i < 16; i++)
            fprintf(stderr, "  %s=%llx%s", nm[i], (unsigned long long)c->r[i], (i % 4 == 3) ? "\n" : "");
    if (c && c->rip) {
        fprintf(stderr, "  bytes@rip:");
        uint8_t *p = (uint8_t *)c->rip;
        for (int i = 0; i < 24; i++)
            fprintf(stderr, " %02x", p[i]);
        fprintf(stderr, "\n");
    }
    if (c) {
        uint64_t pp = c->r[7];
        if (pp > 0x100000000ull && pp < 0x200000000ull) { // rdi: dump chunk header [p-16..p+8)
            fprintf(stderr, "  hdr[rdi-16..p+8):");
            uint8_t *b = (uint8_t *)(pp - 16);
            for (int i = 0; i < 24; i++)
                fprintf(stderr, " %02x", b[i]);
            fprintf(stderr, "  (p-8 u32=%x p-4 u8=%x p-2 u16=%x)\n", *(uint32_t *)(pp - 8), *(uint8_t *)(pp - 4),
                    *(uint16_t *)(pp - 2));
            fprintf(stderr, "  scan-back for group->meta (qword at p-16*off-16):");
            for (int off = 0; off <= 32; off++) {
                uint64_t bv = *(uint64_t *)(pp - 16 * off - 16);
                if (bv > 0x100000000ull && bv < 0x200000000ull)
                    fprintf(stderr, " off=%d->%llx", off, (unsigned long long)bv);
            }
            fprintf(stderr, "\n");
        }
    }
    if (c)
        for (int rr = 0; rr < 16; rr++) { // dump memory at any reg that looks like a heap pointer
            uint64_t v = c->r[rr];
            if (v > 0x100000000ull && v < 0x200000000ull && (v & 7) == 0) {
                fprintf(stderr, "  mem[%d=%llx]:", rr, (unsigned long long)v);
                for (int i = 0; i < 6; i++)
                    fprintf(stderr, " %016llx", (unsigned long long)((uint64_t *)v)[i]);
                fprintf(stderr, "\n");
                if (rr >= 3) break; // a couple is enough
            }
        }
    _exit(133);
}
