// poll on a closed descriptor reports POLLNVAL (and still counts toward the return value).
#include <unistd.h>
#include <poll.h>
#include <fcntl.h>
#include <stdio.h>
int main(void){
    int fd = open("/dev/null", O_RDONLY);
    close(fd);
    struct pollfd pf = { .fd = fd, .events = POLLIN };
    int n = poll(&pf, 1, 0);
    printf("nval n=%d nval=%d\n", n, (pf.revents & POLLNVAL) != 0);   // 1 1
    return 0;
}
