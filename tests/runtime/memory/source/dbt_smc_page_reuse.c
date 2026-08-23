#include "dbt.h"

int main(void) {
    unsigned char *page = dbt_alloc(4096, PROT_READ | PROT_WRITE | PROT_EXEC);
    dbt_emit_ret_imm(page, 7);
    dbt_flush(page, 4096);
    int (*function)(void) = (int (*)(void))page;
    if (function() != 7) return 2;
    if (munmap(page, 4096) != 0) return 3;
    unsigned char *data = mmap(page, 4096, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED, -1, 0);
    if (data != page) return 4;
    data[0] = 11;
    data[1] = 29;
    printf("smc-page-reuse first=%u second=%u\n", data[0], data[1]);
    return data[0] == 11 && data[1] == 29 ? 0 : 5;
}
