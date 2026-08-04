// Main-thread TLS access-model coverage: one __thread variable per access model (local-exec,
// initial-exec, general-dynamic, local-dynamic), each set then re-read after a churn of allocations.
// This is the exact shape of DB::current_thread in clickhouse (#281): a non-PIE ET_EXEC whose
// thread_local pointer (local-exec, TP-relative) must keep its identity across intervening mallocs.
// Kept single-threaded on purpose so the non-PIE oracle build isolates the TLS relocation/model
// translation (the spawned-thread path is covered by the portable, threaded tls-models case).
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

__thread void *v_le __attribute__((tls_model("local-exec")));
__thread void *v_ie __attribute__((tls_model("initial-exec")));
__thread void *v_gd __attribute__((tls_model("global-dynamic")));
__thread void *v_ld __attribute__((tls_model("local-dynamic")));
__thread long  n_le __attribute__((tls_model("local-exec")));

static void churn(void) {
    for (int i = 0; i < 1500; i++) {
        void *m = malloc(48 + (i & 63));
        memset(m, i, 16);
        if (i & 1) free(m);
    }
}

int main(void) {
    long m0, m1, m2, m3;
    v_le = &m0; v_ie = &m1; v_gd = &m2; v_ld = &m3; n_le = 0x5A5A;
    churn();
    int ok = (v_le == &m0) + (v_ie == &m1) + (v_gd == &m2) + (v_ld == &m3) + (n_le == 0x5A5A);
    printf("tlsmodels main=%d\n", ok); // 5
    return ok == 5 ? 0 : 1;
}
