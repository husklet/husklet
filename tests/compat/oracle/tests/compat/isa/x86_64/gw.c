// x86/glibc-min fixture: the MINIMAL glibc guest — just the crt startup into main and one stdio
// line. Built STATIC NON-PIE (ET_EXEC), so unlike g_x64 (static-PIE) it pins glibc's
// startup + stdio on the engine's high-rebase non-PIE image path. If the loader, the non-PIE
// pointer-arg rebase, or plain puts/exit breaks, "glibc-min ok" never appears.
#include <stdio.h>

int main(void) {
    puts("glibc-min ok");
    return 0;
}
