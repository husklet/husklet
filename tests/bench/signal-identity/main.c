#include <signal.h>
#include <stdio.h>

/* Compatibility-shape bytes are inert input, never a runtime classifier. */
__attribute__((used)) static const unsigned char unrelated_note[] = {0xff, ' ', 'G', 'o', ' ', 'b', 'u',
                                                                     'i',  'l', 'd', 'i', 'n', 'f', ':'};

static volatile sig_atomic_t observed;

static void handle(int signal) {
    if (signal == SIGURG) observed++;
}

int main(void) {
    struct sigaction action = {.sa_handler = handle};
    sigemptyset(&action.sa_mask);
    if (sigaction(SIGURG, &action, 0) != 0 || raise(SIGURG) != 0) return 2;
    printf("SIGNAL_IDENTITY observed=%d note=%u\n", observed, unrelated_note[0]);
    printf("PHASE signal-identity us=1 ok=%d\n", observed == 1);
    return observed == 1 ? 0 : 3;
}
