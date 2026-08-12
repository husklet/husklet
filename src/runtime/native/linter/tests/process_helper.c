#include <stdio.h>
#include <stdlib.h>
#include <string.h>

int main(int argc, char **argv) {
    if (argc >= 2 && strcmp(argv[1], "capture") == 0) {
        fputs("helper-stdout\n", stdout);
        fputs("helper-stderr\n", stderr);
        return 37;
    }
    if (argc >= 2 && strcmp(argv[1], "arguments") == 0) {
        for (int i = 2; i < argc; ++i) {
            printf("%d:%zu:%s\n", i - 2, strlen(argv[i]), argv[i]);
        }
        return 0;
    }
    if (argc >= 2 && strcmp(argv[1], "large") == 0) {
        for (int i = 0; i < 1024; ++i) {
            fputc('x', stdout);
        }
        return 0;
    }

    fputs("unknown helper mode\n", stderr);
    return 2;
}
