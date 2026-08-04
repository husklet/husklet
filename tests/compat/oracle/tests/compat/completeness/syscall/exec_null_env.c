#include "compat.h"
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
extern char **environ;
/* Guest-initiated execve makes the passed envp AUTHORITATIVE, exactly like Linux:
 *   - a curated single-var envp yields exactly that one entry (NO PATH/HOME/LANG defaults injected), and
 *   - envp==NULL yields an EMPTY environment (envc==0), not a stale/default inherited env.
 * We chain three self-execs in one process so stdout ordering is deterministic. */
static int envcount(void) {
    int n = 0;
    for (char **e = environ; *e; e++)
        n++;
    return n;
}
int main(int argc, char **argv) {
    if (argc > 1 && strcmp(argv[1], "one") == 0) {
        /* Stage 2: exec'd with a curated single-var env -> Linux passes it verbatim, no injected defaults. */
        const char *v = getenv("ONLYVAR");
        printf("curated envc=%d onlyvar_ok=%d\n", envcount(), (v && strcmp(v, "hello") == 0));
        fflush(stdout);
        char *av[] = {argv[0], (char *)"null", (char *)0};
        execve(argv[0], av, NULL); /* NULL envp -> empty environment on Linux */
        perror("execve null");
        return 1;
    }
    if (argc > 1 && strcmp(argv[1], "null") == 0) {
        /* Stage 3: exec'd with envp==NULL -> environment MUST be empty. */
        printf("null envc=%d\n", envcount());
        fflush(stdout);
        return 0;
    }
    /* Stage 1: exec ourselves with EXACTLY one env var. */
    char *env[] = {(char *)"ONLYVAR=hello", (char *)0};
    char *av[] = {argv[0], (char *)"one", (char *)0};
    fflush(stdout);
    execve(argv[0], av, env);
    perror("execve one");
    return 1;
}
