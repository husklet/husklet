// /proc/cpuinfo — MUST be arch-appropriate for the guest reading it. The binary knows its own arch, so we
// assert the right fields: x86_64 exposes "vendor_id"/"model name"/"flags" (feature detection reads these);
// aarch64 exposes "Features"/"CPU architecture"/"CPU implementer". At least one "processor" block, and the
// processor index must be present. A single arch-form served to both engines is a stub -> fails on x86.
#include <stdio.h>
#include <string.h>
#include "pf.h"

int main(void) {
    char b[8192];
    int n = pf_read("/proc/cpuinfo", b, sizeof b);
    int has_processor = pf_has(b, "processor");
#if defined(__x86_64__)
    int arch_ok = pf_has(b, "vendor_id") && pf_has(b, "model name") && pf_has(b, "flags") &&
                  pf_has(b, "cpu family") && pf_has(b, "fpu");
#elif defined(__aarch64__)
    int arch_ok = pf_has(b, "Features") && pf_has(b, "CPU architecture") && pf_has(b, "CPU implementer");
#else
    int arch_ok = 0;
#endif
    int ok = n > 0 && has_processor && arch_ok;
    printf("cpuinfo ok=%d\n", ok);
    return 0;
}
