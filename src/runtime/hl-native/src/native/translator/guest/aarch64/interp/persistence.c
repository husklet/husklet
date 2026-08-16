// The persistent cache stores host code. A JIT-written file contains AArch64 instructions this backend
// cannot execute on a non-AArch64 host, so loading is a clean miss and saving is a no-op. PCACHE_*_HOOK
// stays undefined so optional hook call sites vanish.
#define PC_IMG_BASE 0x0000040000000000ull    // fixed guest image base
#define PC_INTERP_BASE 0x0000048000000000ull // fixed interpreter (ld.so) base

static int g_pcache; // HL_PCACHE=1 requested (never hits)
static int g_coldprof;
static uint64_t g_force_base;   // one-shot fixed-VA request consumed by load_elf
static int g_force_base_failed; // a fixed-VA map fell back to a kernel base
static hl_identity_digest g_pc_binid; // binary + interp + argv0 + build + host ISA
static uint64_t g_pc_entry;     // initial guest pc
static int g_pcache_loaded;     // never set here
static int g_nreloc;            // always zero here

// Engine-identity mix-in for the cache key. Must be right even though the cache never hits: host_isa is
// HL_HOST_CPU_ISA, not a hardcoded 1 -- passing 1 would collide an x86-64-host identity with a JIT-written
// cache for the same guest, resolved by executing AArch64 on x86-64. Same value keys the checkpoint image.
static uint64_t pcache_engine_id(void) {
    static const char tag[] = __DATE__ " " __TIME__;
    uint64_t build = hl_digest_bytes(HL_DIGEST_SEED, tag, sizeof tag - 1);
    uint64_t self = hl_identity_source(&g_jit_services, g_self_path);
    build = hl_digest_bytes(build, &self, sizeof self);
    // Bit 0 marks "interpreter", so this identity can never equal the JIT's on a shared ISA number.
    uint64_t modes = 1u;
    return hl_identity_configuration(build, HL_HOST_CPU_ISA_AARCH64, HL_HOST_CPU_ISA, modes);
}

static hl_identity_digest pcache_translator_identity(void) {
    static const char tag[] = __DATE__ " " __TIME__;
    return hl_identity_engine_digest(tag, sizeof tag - 1, HL_PCACHE_ABI_AARCH64, HL_HOST_CPU_ISA_AARCH64,
                                     HL_HOST_CPU_ISA, 1u);
}

static hl_identity_digest pcache_make_id(hl_identity_digest program, hl_identity_digest interpreter,
                                        const char *argv0) {
    return hl_identity_digest_mix(program, interpreter, pcache_translator_identity(), argv0);
}

// Always a clean miss.
static int pcache_load(uint64_t entry_jump) {
    (void)entry_jump;
    return 0;
}

// Descriptors are process-local and contain no translated bytes.
static void pcache_save(void) {
}

static void pcache_poison_check(void) {
}

static void pcache_directory_close(void) {
}

static void pcache_note_fixed_img(uint64_t base, uint64_t span) {
    (void)base;
    (void)span;
}
