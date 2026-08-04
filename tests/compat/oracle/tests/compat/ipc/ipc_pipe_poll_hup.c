// poll reports POLLHUP on the read end once every write end is closed; any buffered data is still
// delivered (POLLIN) before the hangup is observed on an empty pipe.
#include <unistd.h>
#include <poll.h>
#include <stdio.h>
int main(void){
    int p[2];
    if (pipe(p)) return 1;
    write(p[1], "ab", 2);
    close(p[1]);
    struct pollfd pf = { .fd = p[0], .events = POLLIN };
    poll(&pf, 1, 100);
    int in = (pf.revents & POLLIN) != 0, hup1 = (pf.revents & POLLHUP) != 0;
    char b[8]; while (read(p[0], b, sizeof b) > 0) {}
    pf.revents = 0; poll(&pf, 1, 100);
    int hup2 = (pf.revents & POLLHUP) != 0;
    printf("pollhup with_data in=%d hup=%d\n", in, hup1);   // 1 1 (POLLIN and HUP can coexist)
    printf("pollhup drained hup=%d\n", hup2);               // 1
    return 0;
}
