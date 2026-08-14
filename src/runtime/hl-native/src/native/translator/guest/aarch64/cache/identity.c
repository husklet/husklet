static uint64_t pcache_id_of(const char *path) {
    return hl_identity_source(&g_jit_services, path);
}

// Per-engine-build tag so the cache self-invalidates across engine rebuilds: host bytes emitted by another
// build must never be loaded, because they are instructions the current translator would not have emitted.
//
// The explicit translator ABI is authoritative for embedded engines, where g_self_path identifies the
// outer application rather than the translator archive. The executable identity and compile-time tag remain
// useful mix-ins, but neither replaces bumping PC_TRANSLATOR_ABI after an incompatible codegen change.
static uint64_t pcache_engine_id(void) {
    static const char tag[] = __DATE__ " " __TIME__;
    uint64_t build = hl_digest_bytes(HL_DIGEST_SEED, tag, sizeof tag - 1);
    build = hl_digest_bytes(build, &(uint64_t){pcache_id_of(g_self_path)}, sizeof(uint64_t));
    build = hl_digest_bytes(build, &(uint64_t){PC_TRANSLATOR_ABI}, sizeof(uint64_t));
    uint64_t modes = (uint64_t)(g_guestfold != 0) | ((uint64_t)(g_steal1617 != 0) << 1) |
                     ((uint64_t)(g_noibslim != 0) << 2) | ((uint64_t)(g_mtibtc != 0) << 3) |
                     ((uint64_t)(g_no_stw_reclaim != 0) << 4) | ((uint64_t)(g_prof != 0) << 5) |
                     ((uint64_t)(uint32_t)g_fwdskip << 32);
    return hl_identity_configuration(build, 1, 1, modes);
}

// Hash the BASENAME of argv[0]. A multicall binary (busybox, toolchain drivers) runs DIFFERENT code
// paths per argv[0]; the translated arena is therefore per-applet, so the cache MUST be keyed by argv[0]
// too or one applet loads another's arena. Basename (not full argv) so a single-purpose binary invoked
// with varying flags -- e.g. go's `compile -o pkgN.a -p pkgN ...` -- keeps ONE cache reused across all
// its invocations (the go-build fork-storm win).
static uint64_t pcache_argv0_id(const char *argv0) {
    return hl_identity_name(argv0);
}

static hl_identity_digest pcache_make_id(hl_identity_digest program, hl_identity_digest interpreter,
                                        const char *argv0) {
    return hl_identity_digest_mix(program, interpreter, pcache_engine_id(), pcache_argv0_id(argv0));
}

static int pcache_file(char *out, size_t n) {
    const char *dir = hl_option_get("HL_PCACHE_DIR");
    if (!dir || !dir[0]) dir = "/tmp/hl-engine-pcache-aarch64";
    if (g_pc_directory.handle != HL_HOST_HANDLE_INVALID && strcmp(g_pc_directory_path, dir) != 0) {
        (void)hl_persist_directory_close(&g_pc_directory);
        g_pc_directory_path[0] = 0;
    }
    if (g_pc_directory.handle == HL_HOST_HANDLE_INVALID &&
        !hl_persist_directory_open(&g_pc_directory, &g_jit_services, dir, 1))
        return 0;
    if (!g_pc_directory_path[0]) {
        int copied = snprintf(g_pc_directory_path, sizeof g_pc_directory_path, "%s", dir);
        if (copied <= 0 || (size_t)copied >= sizeof g_pc_directory_path) {
            (void)hl_persist_directory_close(&g_pc_directory);
            g_pc_directory_path[0] = 0;
            return 0;
        }
    }
    static const char hex[] = "0123456789abcdef";
    if (n < 72) return 0;
    for (size_t i = 0; i < sizeof g_pc_binid.bytes; ++i) {
        out[i * 2] = hex[g_pc_binid.bytes[i] >> 4];
        out[i * 2 + 1] = hex[g_pc_binid.bytes[i] & 15];
    }
    memcpy(out + 64, ".pcache", 8);
    return 1;
}
