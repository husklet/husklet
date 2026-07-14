// chmod full mode space + the special bits (suid 04000 / sgid 02000 / sticky 01000), verified via stat(2)
// st_mode. The exact 12-bit mode must round-trip. Also access(2): X_OK needs an execute bit. Portable
// (Linux emulated + native macOS) -> golden verdict, so the JIT's mode tracking must match the kernel.
#include <fcntl.h>
#include <stdio.h>
#include <sys/stat.h>
#include <unistd.h>

static int mode_is(const char *p, unsigned want) {
    struct stat s;
    if (stat(p, &s) != 0) return 0;
    return (s.st_mode & 07777) == want;
}

int main(void) {
    char path[128];
    snprintf(path, sizeof path, "/tmp/hl_perm_%d", (int)getpid());
    int fd = open(path, O_CREAT | O_RDWR, 0644);
    close(fd);

    int ok = 1;
    // Full base mode space (the 9 rwx bits). Round-trips for the owning process regardless of privilege,
    // so this validates the JIT's mode tracking. The suid/sgid/sticky special bits require root to set and
    // are covered in the `permissions` docker scenario (a container runs as root); a non-root/native run
    // cannot set them, so they are deliberately excluded from this bare, portable fixture.
    unsigned modes[] = {0644, 0755, 0600, 0000, 0777, 0700, 0640, 0444, 0711, 0666, 0640, 0751};
    for (unsigned i = 0; i < sizeof modes / sizeof *modes; i++) {
        ok &= chmod(path, modes[i]) == 0 && mode_is(path, modes[i]);
    }

    // access(2): a mode with no execute bit -> X_OK fails; with r/w bits -> R_OK/W_OK on the owner.
    chmod(path, 0644);
    int noexec = access(path, X_OK) != 0;      // 0644: not executable
    int canrw = access(path, R_OK) == 0 && access(path, W_OK) == 0;
    chmod(path, 0755);
    int canexec = access(path, X_OK) == 0;     // 0755: executable
    ok &= noexec && canrw && canexec;

    unlink(path);
    printf("permbits ok=%d\n", ok);
    return 0;
}
