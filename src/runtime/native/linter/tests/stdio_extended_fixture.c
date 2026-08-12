#include <stdio.h>

void hl_lint_extended_stdio_fixture(void *arguments) {
    dprintf(1, "descriptor output\n");
    vdprintf(2, "descriptor output\n", arguments);
    fputc('x', stdout);
    putchar('x');
}
