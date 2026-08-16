extern int g_rwx_guest;

#define SMC_MAX 65536
#define SMC_INDEX_SLOTS (SMC_MAX * 2)
static uint64_t g_smc_pg[SMC_MAX];
static _Atomic uint64_t g_smc_index_slots[SMC_INDEX_SLOTS];
static hl_smc_page_index g_smc_index = {g_smc_index_slots, SMC_INDEX_SLOTS};
static int g_smc_n;
static uint64_t g_smc_flushes;

static uint64_t smc_page_size(void) {
    static uint64_t size;
    if (size == 0) size = (uint64_t)getpagesize();
    return size;
}

static void smc_protect(uint64_t pc) {
    if (!g_rwx_guest) return;
    const void *canonical = NULL;
    size_t contiguous = 0;
    int resolved = hl_guest_memory_resolve_exec(pc, 1, &canonical, &contiguous);
    if (resolved < 0 || !hl_smc_address_is_direct(resolved)) return;
    uint64_t size = smc_page_size();
    uint64_t page = pc & ~(size - 1);
    for (int index = 0; index < g_smc_n; ++index)
        if (g_smc_pg[index] == page) return;
    if (g_smc_n >= SMC_MAX) return;
    hl_smc_page_index_add_result indexed = hl_smc_page_index_add(&g_smc_index, page);
    if (indexed == HL_SMC_PAGE_INDEX_FULL || indexed == HL_SMC_PAGE_INDEX_EXISTS) return;
    if (mprotect((void *)page, (size_t)size, PROT_READ) != 0) {
        if (indexed == HL_SMC_PAGE_INDEX_INSERTED) (void)hl_smc_page_index_remove(&g_smc_index, page);
        return;
    }
    g_smc_pg[g_smc_n++] = page;
}

static int smc_on_write(uint64_t address) {
    if (!g_rwx_guest) return 0;
    uint64_t size = smc_page_size();
    uint64_t page = address & ~(size - 1);
    if (!hl_smc_page_index_contains(&g_smc_index, page)) return 0;
    mprotect((void *)page, (size_t)size, PROT_READ | PROT_WRITE);
    ++g_smc_flushes;
    return 1;
}

static int smc_tracked_written(uint64_t address, uint64_t size) {
    if (size == 0 || address > UINT64_MAX - size) return 0;
    uint64_t page_size = smc_page_size();
    uint64_t first = address & ~(page_size - 1);
    uint64_t last = (address + size - 1) & ~(page_size - 1);
    for (uint64_t guest_page = first;; guest_page += page_size) {
        uint64_t page = hl_x86_guest_pointer(guest_page) & ~(page_size - 1);
        if (hl_smc_page_index_contains(&g_smc_index, page)) {
            (void)mprotect((void *)page, (size_t)page_size, PROT_READ);
            return 1;
        }
        if (guest_page == last) break;
    }
    return 0;
}
