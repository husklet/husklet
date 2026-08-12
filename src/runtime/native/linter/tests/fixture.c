#include <stdio.h>

int hl_lint_fixture(FILE *report) {
    char buffer[32];
    int printf_count = snprintf(buffer, sizeof buffer, "%s", "printf(");
    // printf("comments are not calls");
    if (report != NULL) { fprintf(report, "%s", buffer); }
    return printf_count;
}
