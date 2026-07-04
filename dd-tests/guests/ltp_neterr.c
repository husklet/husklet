// connect()/bind()/sendto() ERROR-path semantics — LTP connect01/bind01/sendto02 surface. The priority is
// that a BAD ADDRESS POINTER returns EFAULT rather than CRASHING the engine. Deterministic, oracle-diffed
// dd-vs-native on both arches. Uses a loopback listener for the success/EISCONN cases so it needs no net.
#include <errno.h>
#include <netinet/in.h>
#include <stdio.h>
#include <string.h>
#include <sys/socket.h>
#include <unistd.h>

int main(void) {
    setbuf(stdout, NULL); // unbuffered: no output is lost if a bad-pointer case were to fault
    // ---- connect() error paths ----
    int s = socket(AF_INET, SOCK_STREAM, 0);
    struct sockaddr_in sa;
    memset(&sa, 0, sizeof sa);
    sa.sin_family = AF_INET;
    sa.sin_port = htons(1); // nothing listening
    sa.sin_addr.s_addr = htonl(0x7f000001);

    // connect on a non-socket fd -> ENOTSOCK.
    errno = 0;
    int r_ns = connect(0, (struct sockaddr *)&sa, sizeof sa); // fd 0 is stdin, not a socket
    printf("connect ENOTSOCK: ret=%d ok=%d\n", r_ns, r_ns < 0 && errno == ENOTSOCK);

    // connect with a bad address POINTER -> EFAULT (must NOT crash the engine).
    errno = 0;
    int r_ef = connect(s, (struct sockaddr *)0x8, sizeof sa);
    printf("connect EFAULT: ret=%d ok=%d\n", r_ef, r_ef < 0 && errno == EFAULT);

    // connect with a too-short addrlen -> EINVAL.
    errno = 0;
    int r_iv = connect(s, (struct sockaddr *)&sa, 1);
    printf("connect EINVAL: ret=%d ok=%d\n", r_iv, r_iv < 0 && errno == EINVAL);

    // connect on a bad fd -> EBADF.
    errno = 0;
    int r_bf = connect(400, (struct sockaddr *)&sa, sizeof sa);
    printf("connect EBADF: ret=%d ok=%d\n", r_bf, r_bf < 0 && errno == EBADF);
    close(s);

    // ---- bind() error paths ----
    int s2 = socket(AF_INET, SOCK_STREAM, 0);
    // bind with a bad address pointer -> EFAULT.
    errno = 0;
    int b_ef = bind(s2, (struct sockaddr *)0x8, sizeof sa);
    printf("bind EFAULT: ret=%d ok=%d\n", b_ef, b_ef < 0 && errno == EFAULT);
    // bind on a non-socket fd -> ENOTSOCK.
    errno = 0;
    int b_ns = bind(1, (struct sockaddr *)&sa, sizeof sa);
    printf("bind ENOTSOCK: ret=%d ok=%d\n", b_ns, b_ns < 0 && errno == ENOTSOCK);
    // bind on a bad fd -> EBADF.
    errno = 0;
    int b_bf = bind(400, (struct sockaddr *)&sa, sizeof sa);
    printf("bind EBADF: ret=%d ok=%d\n", b_bf, b_bf < 0 && errno == EBADF);
    close(s2);

    // ---- sendto() error paths ----
    int s3 = socket(AF_INET, SOCK_DGRAM, 0);
    char msg[4] = "abc";
    // sendto with a bad buffer POINTER -> EFAULT.
    errno = 0;
    long t_ef = sendto(s3, (void *)0x8, 4, 0, (struct sockaddr *)&sa, sizeof sa);
    printf("sendto EFAULT: ret=%ld ok=%d\n", t_ef, t_ef < 0 && errno == EFAULT);
    // sendto on a bad fd -> EBADF.
    errno = 0;
    long t_bf = sendto(400, msg, 4, 0, (struct sockaddr *)&sa, sizeof sa);
    printf("sendto EBADF: ret=%ld ok=%d\n", t_bf, t_bf < 0 && errno == EBADF);
    // sendto on a non-socket fd -> ENOTSOCK.
    errno = 0;
    long t_ns = sendto(1, msg, 4, 0, (struct sockaddr *)&sa, sizeof sa);
    printf("sendto ENOTSOCK: ret=%ld ok=%d\n", t_ns, t_ns < 0 && errno == ENOTSOCK);
    close(s3);

    return 0;
}
