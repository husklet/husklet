// Container read-only rootfs fidelity (docker --read-only). A write to the rootfs must fail EROFS, while
// the writable pseudo-mount /tmp stays writable -- exactly as runc leaves /tmp a tmpfs over a ro root. Used
// for the darwin engine (native under darwinjail, whose interposers return EROFS); the Linux engines assert
// the same behaviour through a busybox shell in a real image rootfs (see cases/ext/isolation.rs).
#include <stdio.h>
#include <fcntl.h>
#include <errno.h>
#include <unistd.h>

int main(void) {
    // write into the rootfs root -> EROFS under --read-only
    errno = 0;
    int a = open("/dd_ro_probe", O_WRONLY | O_CREAT | O_TRUNC, 0644);
    int ae = errno;
    if (a >= 0) {
        close(a);
        unlink("/dd_ro_probe");
    }
    printf("root=%s ", a >= 0 ? "RW" : (ae == EROFS ? "EROFS" : "ERR"));

    // /tmp is a writable pseudo-mount even under --read-only
    int b = open("/tmp/dd_ro_probe", O_WRONLY | O_CREAT | O_TRUNC, 0644);
    if (b >= 0) {
        close(b);
        unlink("/tmp/dd_ro_probe");
    }
    printf("tmp=%s\n", b >= 0 ? "OK" : "FAIL");
    return 0;
}
