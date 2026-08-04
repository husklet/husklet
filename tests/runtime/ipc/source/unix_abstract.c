// Abstract-namespace AF_UNIX sockets (leading NUL in sun_path) bind/connect without a filesystem
// entry and carry a stream payload end to end.
#include <sys/socket.h>
#include <sys/un.h>
#include <unistd.h>
#include <string.h>
#include <stdio.h>
int main(void){
    int ls = socket(AF_UNIX, SOCK_STREAM, 0);
    struct sockaddr_un a; memset(&a, 0, sizeof a);
    a.sun_family = AF_UNIX;
    const char *name = "hl-compat-abstract";
    a.sun_path[0] = '\0';
    memcpy(a.sun_path + 1, name, strlen(name));
    socklen_t alen = offsetof(struct sockaddr_un, sun_path) + 1 + strlen(name);
    int b = bind(ls, (struct sockaddr *)&a, alen);
    listen(ls, 1);
    int cs = socket(AF_UNIX, SOCK_STREAM, 0);
    int c = connect(cs, (struct sockaddr *)&a, alen);
    int as = accept(ls, NULL, NULL);
    write(cs, "abs", 3);
    char buf[3] = {0}; ssize_t n = read(as, buf, 3);
    printf("abstract bind=%d connect=%d recv=%zd data=%.3s\n", b, c, n, buf);  // 0 0 3 abs
    return 0;
}
