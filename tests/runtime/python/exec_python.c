#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>

int main(int argc, char **argv) {
    static const char interpreter[] = "/usr/local/bin/python3";
    char **arguments = calloc((size_t)argc + 1, sizeof(*arguments));
    if (arguments == NULL) {
        fputs("exec-python: allocation failed\n", stderr);
        return 125;
    }
    arguments[0] = (char *)interpreter;
    for (int index = 1; index < argc; ++index) {
        arguments[index] = argv[index];
    }
    execv(interpreter, arguments);
    const int error = errno;
    fprintf(stderr, "exec-python: %s: ", interpreter);
    errno = error;
    perror(NULL);
    free(arguments);
    return 127;
}
