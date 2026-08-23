enum ckpt_outstanding_kind {
    CKPT_OUTSTANDING_MISSED_SAFEPOINT = 1,
    CKPT_OUTSTANDING_DUMP_REFUSED = 2,
    CKPT_OUTSTANDING_STALE_MEMBER = 3,
    CKPT_OUTSTANDING_DIED_AFTER_JOIN = 4,
    CKPT_OUTSTANDING_UNKNOWN = 5,
};

/* Classify an incomplete member without changing the rendezvous decision.  The old refusal combined
 * "never reached a safepoint" and "dump refused" in one sentence, after which teardown removed the only
 * /proc evidence that could distinguish them.  Liveness and the broker's REGISTER_READY ledger are the
 * two independent facts: together they also expose a stale enumerated corpse instead of reporting it as a
 * blocked live process. */
static enum ckpt_outstanding_kind ckpt_outstanding_classify(int live, int registered) {
    if (registered < 0) return CKPT_OUTSTANDING_UNKNOWN;
    if (live) return registered ? CKPT_OUTSTANDING_DUMP_REFUSED : CKPT_OUTSTANDING_MISSED_SAFEPOINT;
    return registered ? CKPT_OUTSTANDING_DIED_AFTER_JOIN : CKPT_OUTSTANDING_STALE_MEMBER;
}

static const char *ckpt_outstanding_describe(enum ckpt_outstanding_kind kind) {
    switch (kind) {
    case CKPT_OUTSTANDING_MISSED_SAFEPOINT: return "live but never registered (missed safepoint)";
    case CKPT_OUTSTANDING_DUMP_REFUSED: return "live and registered (dump did not commit)";
    case CKPT_OUTSTANDING_STALE_MEMBER: return "dead and unregistered (stale enumeration member)";
    case CKPT_OUTSTANDING_DIED_AFTER_JOIN: return "dead after registering (died during dump)";
    default: return "membership unknown (broker query failed)";
    }
}

#if defined(HL_NATIVE_TEST_HOOKS)
static int ckpt_outstanding_test(uint32_t scenario) {
    static const int facts[][2] = {{1, 0}, {1, 1}, {0, 0}, {0, 1}};
    return ckpt_outstanding_classify(facts[scenario - 4][0], facts[scenario - 4][1]) ==
                   (enum ckpt_outstanding_kind)(scenario - 3)
               ? 0
               : -1;
}
#endif
