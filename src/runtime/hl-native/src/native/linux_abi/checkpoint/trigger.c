static volatile uint32_t *ckpt_map_trigger_descriptor(int fd) {
    void *m = mmap(NULL, 4, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    if (m == MAP_FAILED) fprintf(stderr, "[ckpt] cannot map inherited trigger: %s\n", strerror(errno));
    return (m == MAP_FAILED) ? NULL : (volatile uint32_t *)m;
}

// The generation counter is an anonymous shared descriptor inherited from activation: one shared word, read
// by ckpt_poll at every safepoint, bumped by the embedder to request a capture.
static volatile uint32_t *ckpt_map_trigger(void) {
    int inherited = hl_ckpt_trigger_descriptor();
    if (inherited < 0) {
        fprintf(stderr, "[ckpt] checkpoint requested without a trigger descriptor\n");
        return NULL;
    }
    return ckpt_map_trigger_descriptor(inherited);
}

// A restored child rebuilds its guest address space with MAP_FIXED. It inherited
// the parent's trigger mapping at an address chosen for the parent's layout;
// that address can belong to the child's saved guest image. Detach it before
// replay so MAP_FIXED cannot silently replace engine state, then map the same
// shared descriptor again after the guest topology owns all of its addresses.
static int ckpt_trigger_detach_for_restore(void) {
    if (g_ckpt_trigger == NULL) return 0;
    if (munmap((void *)g_ckpt_trigger, sizeof *g_ckpt_trigger) != 0) return -1;
    g_ckpt_trigger = NULL;
    return 1;
}

static int ckpt_trigger_reattach_after_restore(int detached) {
    if (!detached) return 0;
    if (hl_option_get("HL_CKPT_TEST_FAIL_TRIGGER_REATTACH") != NULL) return -1;
    g_ckpt_trigger = ckpt_map_trigger();
    return g_ckpt_trigger == NULL ? -1 : 0;
}
