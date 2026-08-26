#include <stdio.h>
#include <stdlib.h>
#include <string.h>

int main(int argc, char **argv) {
    if (argc > 1 && !strcmp(argv[1], "output")) {
        fputs("native-supervised", stdout);
        return 23;
    }
    return 0;
}
