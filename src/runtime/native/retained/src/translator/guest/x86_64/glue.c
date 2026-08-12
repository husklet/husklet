#include "glue.h"

int g_trace;
int g_noibtc;
int g_itrace;
uint64_t g_emit_gpc;
int g_systrace;
uint64_t g_disp_n;
uint64_t g_ibtc_fill;
uint64_t g_repmovs_n;
uint64_t g_repstos_n;
hl_x86_ibtc_entry g_xibtc[XIBTC_SETS * XIBTC_WAYS];
int g_coldprof;
int g_pcache;
int g_pcache_loaded;
uint64_t g_pc_binid;
uint64_t g_pc_entry;
uint64_t g_force_base;
hl_reloc g_reloc_storage[PC_RELOC_CAP];
hl_reloc_table g_reloc_table = {g_reloc_storage, 0, (int)PC_RELOC_CAP};
int g_pcache_poison;
uint64_t g_tracecap;
int g_diag;
int g_nochain;
int g_dbg_nochain;
int g_dbg_gprdump;
uint64_t g_loadbase;
uint8_t *g_w8;
uint8_t g_w8v;
uint64_t g_malloc_n;
const char *g_exe_path = "";
const char *g_self_path = "";
uint64_t g_pmovmskb_n;
int g_notier2x = -1;
uint64_t g_prof_t2fold;
uint64_t g_prof_xflag;
uint64_t g_prof_xflag_scan;

int ibtc1way(void) {
    return 0;
}

uint64_t coldprof_now_ns(const hl_host_services *services) {
    hl_host_result result;

    if (services == NULL || services->clock == NULL || services->clock->monotonic_ns == NULL) return 0;
    result = services->clock->monotonic_ns(services->context);
    return result.status == HL_STATUS_OK ? result.value : 0;
}

int nosseopt(void) {
    return 0;
}

int noeaopt(void) {
    return 0;
}

void hl_x86_count_rep_movs(void) {
    g_repmovs_n++;
}

void hl_x86_count_rep_stos(void) {
    g_repstos_n++;
}

int notier2x(void) {
    return g_notier2x;
}
