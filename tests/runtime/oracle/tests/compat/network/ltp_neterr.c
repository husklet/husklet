// Permanent guard for the connect(2)/bind(2) errno matrix that upstream LTP's connect01 + bind01 check.
// hl emulates the Linux socket ABI on top of macOS BSD sockets, and macOS reports a DIFFERENT errno than
// Linux for several bad inputs; net.c's net_precheck() re-derives the Linux errno + ORDER up front. This
// guest asserts every case against the exact Linux errno (arch-independent numbers) so a regression in
// that path fails the matrix on both engines. Golden-compared (deterministic); see cases/ext/net.rs.
//
// The values mirror connect01 (EBADF/EFAULT/EINVAL/ENOTSOCK/EISCONN/ECONNREFUSED/EAFNOSUPPORT) and
// bind01 (EINVAL/ENOTSOCK/EAFNOSUPPORT/EADDRNOTAVAIL/EBADF). EFAULT is exercised with a mmap(PROT_NONE)
// guard page exactly as LTP's tst_get_bad_addr does — the case hl force-maps host-readable, so net.c
// consults its guest-PROT_NONE registry to still fault.
#include <errno.h>
#include <fcntl.h>
#include <netinet/in.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/socket.h>
#include <sys/un.h>
#include <sys/wait.h>
#include <unistd.h>

// Run one connect()/bind() and return the errno it failed with (0 if it unexpectedly succeeded).
static int err_of(int rc) { return rc < 0 ? errno : 0; }

int main(void) {
    void *bad = mmap(0, 1, PROT_NONE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);

    // A real, currently-listening loopback server (for the EISCONN case) + its bound address.
    int srv = socket(AF_INET, SOCK_STREAM, 0);
    struct sockaddr_in la = {0};
    la.sin_family = AF_INET;
    la.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    la.sin_port = 0;
    bind(srv, (struct sockaddr *)&la, sizeof la);
    listen(srv, 8);
    socklen_t ll = sizeof la;
    getsockname(srv, (struct sockaddr *)&la, &ll); // la now holds the live port

    pid_t pid = fork();
    if (pid == 0) {
        int a = accept(srv, NULL, NULL);
        if (a >= 0) close(a);
        _exit(0);
    }
    int conn = socket(AF_INET, SOCK_STREAM, 0);
    connect(conn, (struct sockaddr *)&la, sizeof la); // now connected -> a 2nd connect is EISCONN
    waitpid(pid, NULL, 0);

    // An address whose port nobody listens on (bind an ephemeral socket, read its port, close it).
    int tmp = socket(AF_INET, SOCK_STREAM, 0);
    struct sockaddr_in dead = {0};
    dead.sin_family = AF_INET;
    dead.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    dead.sin_port = 0;
    bind(tmp, (struct sockaddr *)&dead, sizeof dead);
    socklen_t dl = sizeof dead;
    getsockname(tmp, (struct sockaddr *)&dead, &dl);
    close(tmp); // port is now free -> connecting to it is refused

    struct sockaddr_in ok = {0}; // a well-formed AF_INET addr (the live server)
    ok.sin_family = AF_INET;
    ok.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    ok.sin_port = la.sin_port;

    struct sockaddr_in badfam = {0}; // wrong sa_family
    badfam.sin_family = 47;
    badfam.sin_addr.s_addr = htonl(0x0AFFFEFD);

    struct sockaddr_in nonlocal = {0}; // an address not assigned to any local interface
    nonlocal.sin_family = AF_INET;
    nonlocal.sin_addr.s_addr = htonl(0x0AFFFEFD); // 10.255.254.253
    nonlocal.sin_port = 0;

    struct sockaddr_un un = {0}; // AF_UNIX address handed to an AF_INET socket
    un.sun_family = AF_UNIX;
    strncpy(un.sun_path, "/tmp/ltp_neterr.sock", sizeof un.sun_path - 1);

    int fdnull = open("/dev/null", O_WRONLY); // a valid fd that is not a socket
    int s = socket(AF_INET, SOCK_STREAM, 0);  // a fresh, unconnected INET stream socket

    // ---- connect(2) matrix (order + value must match Linux) ----
    // EBADF is exercised with a bad fd and a VALID address (exactly like LTP connect01's tcases[0],
    // .fd=&fd_invalid .addr=&sock1). A bad fd + a bad ADDRESS is deliberately NOT combined: that case is
    // order-ambiguous across implementations — the real Linux kernel checks the fd first (fdget -> EBADF,
    // before move_addr_to_kernel ever reads the sockaddr), while qemu-user copies the sockaddr in userspace
    // BEFORE issuing the host syscall, so it reports EFAULT. hl matches the real kernel (fd first). Passing a
    // valid address here keeps the assertion portable and meaningful (EBADF on every implementation).
    int c_ebadf = err_of(connect(-1, (struct sockaddr *)&ok, sizeof ok));
    int c_efault = err_of(connect(s, (struct sockaddr *)bad, sizeof(struct sockaddr_in)));
    int c_einval = err_of(connect(s, (struct sockaddr *)&ok, 3));
    int c_enotsock = err_of(connect(fdnull, (struct sockaddr *)&ok, sizeof ok));
    int c_eisconn = err_of(connect(conn, (struct sockaddr *)&ok, sizeof ok));
    int c_erefused = err_of(connect(s, (struct sockaddr *)&dead, sizeof dead));
    int c_eafnos = err_of(connect(s, (struct sockaddr *)&badfam, sizeof badfam));

    printf("ltp_neterr connect %d %d %d %d %d %d %d\n", c_ebadf, c_efault, c_einval, c_enotsock, c_eisconn,
           c_erefused, c_eafnos);

    // ---- bind(2) matrix ----
    int b_sock = socket(AF_INET, SOCK_STREAM, 0);
    int b_einval = err_of(bind(b_sock, (struct sockaddr *)&ok, 3));
    int b_enotsock = err_of(bind(fdnull, (struct sockaddr *)&ok, sizeof ok));
    int b_eafnos = err_of(bind(b_sock, (struct sockaddr *)&un, sizeof un));
    int b_eaddrna = err_of(bind(b_sock, (struct sockaddr *)&nonlocal, sizeof nonlocal));
    int b_ebadf = err_of(bind(-1, (struct sockaddr *)&ok, sizeof ok));

    printf("ltp_neterr bind %d %d %d %d %d\n", b_einval, b_enotsock, b_eafnos, b_eaddrna, b_ebadf);
    return 0;
}
