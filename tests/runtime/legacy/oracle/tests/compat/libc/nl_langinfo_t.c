// nl_langinfo in the C locale returns stable identifiers. Portable verdicts.
#include <stdio.h>
#include <langinfo.h>
#include <locale.h>
#include <string.h>

int main(void) {
    setlocale(LC_ALL, "C");
    int d1 = strcmp(nl_langinfo(CODESET), "ANSI_X3.4-1968") == 0;
    int d2 = strcmp(nl_langinfo(DAY_1), "Sunday") == 0;
    int d3 = strcmp(nl_langinfo(ABMON_1), "Jan") == 0;
    int d4 = strcmp(nl_langinfo(D_T_FMT), "%a %b %e %H:%M:%S %Y") == 0;
    int d5 = strcmp(nl_langinfo(RADIXCHAR), ".") == 0;
    printf("nl_langinfo d1=%d d2=%d d3=%d d4=%d d5=%d\n", d1, d2, d3, d4, d5);
    return 0;
}
