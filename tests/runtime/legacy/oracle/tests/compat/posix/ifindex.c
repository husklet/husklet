// if_nametoindex(lo) is a stable positive index and if_indextoname round-trips back to "lo".
#include <net/if.h>
#include <string.h>
#include <stdio.h>

int main(void) {
    unsigned idx = if_nametoindex("lo");
    int has_lo = idx > 0;
    char name[IF_NAMESIZE] = {0};
    int back = has_lo && if_indextoname(idx, name) != NULL && strcmp(name, "lo") == 0;
    // A nonexistent interface returns 0.
    unsigned bad = if_nametoindex("hl_nope0");
    printf("ifindex has_lo=%d back=%d bad=%d\n", has_lo, back, bad == 0);
    return 0;
}
