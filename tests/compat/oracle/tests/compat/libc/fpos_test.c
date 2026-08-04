// fgetpos/fsetpos + fseek SEEK_END/CUR + rewind on a tmpfile. Portable verdicts.
#include <stdio.h>
#include <string.h>

int main(void) {
    FILE *f = tmpfile();
    fputs("0123456789", f);
    fflush(f);
    fseek(f, 3, SEEK_SET);
    fpos_t pos; fgetpos(f, &pos);
    int c1 = fgetc(f); // '3'
    fseek(f, 0, SEEK_END);
    int d1 = ftell(f) == 10;
    fsetpos(f, &pos); // back to offset 3
    int c2 = fgetc(f); // '3'
    int d2 = c1 == '3' && c2 == '3';
    fseek(f, -2, SEEK_END);
    int d3 = fgetc(f) == '8';
    rewind(f);
    int d4 = fgetc(f) == '0';
    fclose(f);
    printf("fpos_test d1=%d d2=%d d3=%d d4=%d\n", d1, d2, d3, d4);
    return 0;
}
