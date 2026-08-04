// confstr(_CS_PATH) returns a non-empty search path; two-call sizing pattern works.
#include <unistd.h>
#include <string.h>
#include <stdlib.h>
#include <stdio.h>

int main(void) {
    size_t need = confstr(_CS_PATH, NULL, 0);
    int sized = need > 0;
    char *buf = malloc(need ? need : 1);
    size_t got = confstr(_CS_PATH, buf, need);
    int filled = got == need && strlen(buf) == need - 1 && strchr(buf, '/') != NULL;
    free(buf);
    printf("confstrt sized=%d filled=%d\n", sized, filled);
    return 0;
}
