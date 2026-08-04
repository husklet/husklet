// atexit handlers run in reverse registration order. Deterministic output.
#include <stdio.h>
#include <stdlib.h>

static void a(void) { fputs("A", stdout); }
static void b(void) { fputs("B", stdout); }
static void c(void) { fputs("C", stdout); }

int main(void) {
    atexit(a); atexit(b); atexit(c);
    fputs("main;", stdout);
    fflush(stdout);
    return 0; // handlers print C,B,A
}
