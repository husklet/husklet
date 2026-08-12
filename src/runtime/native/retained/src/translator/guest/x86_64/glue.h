#ifndef HL_TRANSLATOR_GUEST_X86_64_GLUE_H
#define HL_TRANSLATOR_GUEST_X86_64_GLUE_H

#include "../../reloc.h"
#include "hl/host_services.h"

#include <stdint.h>

#define XIBTC_SETS 8192
#define XIBTC_WAYS 2
#define PC_RELOC_CAP (1u << 20)

enum { PRELOC_BLOCKRET = 1, PRELOC_IBTC = 2, PRELOC_HOSTGLOBAL = 3 };

typedef struct hl_x86_ibtc_entry {
    uint64_t target;
    void *body;
} hl_x86_ibtc_entry;

extern int g_trace;
extern int g_noibtc;
extern int g_itrace;
extern uint64_t g_emit_gpc;
extern int g_systrace;
extern uint64_t g_disp_n;
extern uint64_t g_ibtc_fill;
extern uint64_t g_repmovs_n;
extern uint64_t g_repstos_n;
extern hl_x86_ibtc_entry g_xibtc[XIBTC_SETS * XIBTC_WAYS];
extern int g_coldprof;
extern int g_pcache;
extern int g_pcache_loaded;
extern uint64_t g_pc_binid;
extern uint64_t g_pc_entry;
extern uint64_t g_force_base;
extern hl_reloc g_reloc_storage[PC_RELOC_CAP];
extern hl_reloc_table g_reloc_table;
#define g_reloc (g_reloc_table.records)
#define g_nreloc (g_reloc_table.count)
extern int g_pcache_poison;
extern uint64_t g_tracecap;
extern int g_diag;
extern int g_nochain;
extern int g_dbg_nochain;
extern int g_dbg_gprdump;
extern uint64_t g_loadbase;
extern uint8_t *g_w8;
extern uint8_t g_w8v;
extern uint64_t g_malloc_n;
extern const char *g_exe_path;
extern const char *g_self_path;
extern uint64_t g_pmovmskb_n;
extern int g_notier2x;
extern uint64_t g_prof_t2fold;
extern uint64_t g_prof_xflag;
extern uint64_t g_prof_xflag_scan;

int ibtc1way(void);
uint64_t coldprof_now_ns(const hl_host_services *services);
int nosseopt(void);
int noeaopt(void);
int notier2x(void);
void hl_x86_count_rep_movs(void);
void hl_x86_count_rep_stos(void);

#endif
