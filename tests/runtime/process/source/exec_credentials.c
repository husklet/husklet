#define _GNU_SOURCE
#include <errno.h>
#include <elf.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/auxv.h>
#include <sys/prctl.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <sys/xattr.h>
#include <unistd.h>

#define CAP_BIT (1ULL << 10)
#define FILECAP_DIRECTORY "/tmp/hl-exec-credentials-dir"
#define FILECAP_PATH FILECAP_DIRECTORY "/filecap"
#define MALFORMED_PATH "/tmp/hl-exec-credentials-malformed"
#define UNKNOWN_FLAGS_PATH "/tmp/hl-exec-credentials-unknown-flags"
#define BAD_FORMAT_PATH "/tmp/hl-exec-credentials-bad-format"
#define BAD_INTERPRETER_PATH "/tmp/hl-exec-credentials-bad-interpreter"
#define INTERPRETED_PATH "/tmp/hl-exec-credentials-interpreted"

struct chdr {
    unsigned version;
    int pid;
};

struct cdata {
    unsigned eff, prm, inh;
};

static int caps(unsigned long long *eff, unsigned long long *prm, unsigned long long *inh) {
    struct chdr h = {0x20080522u, 0};
    struct cdata d[2] = {{0}};
    if (syscall(SYS_capget, &h, d) != 0) return 0;
    *eff = ((unsigned long long)d[1].eff << 32) | d[0].eff;
    *prm = ((unsigned long long)d[1].prm << 32) | d[0].prm;
    *inh = ((unsigned long long)d[1].inh << 32) | d[0].inh;
    return 1;
}

static int setcaps(unsigned long long eff, unsigned long long prm, unsigned long long inh) {
    struct chdr h = {0x20080522u, 0};
    struct cdata d[2] = {{0}};
    d[0] = (struct cdata){(unsigned)eff, (unsigned)prm, (unsigned)inh};
    d[1] = (struct cdata){(unsigned)(eff >> 32), (unsigned)(prm >> 32), (unsigned)(inh >> 32)};
    return (int)syscall(SYS_capset, &h, d);
}

static int post(const char *mode) {
    uid_t r, e, s;
    gid_t gr, ge, gs;
    if (getresuid(&r, &e, &s) || getresgid(&gr, &ge, &gs)) return 10;
    if (mode[0] == 'o') {
        errno = 0;
        int regain = setuid(0);
        return r == 1000 && e == 1000 && s == 1000 && gr == 1000 && ge == 1000 && gs == 1000 && regain == -1 &&
                       errno == EPERM && getauxval(AT_UID) == 1000 && getauxval(AT_EUID) == 1000 &&
                       getauxval(AT_GID) == 1000 && getauxval(AT_EGID) == 1000 && getauxval(AT_SECURE) == 0 &&
                       prctl(PR_GET_DUMPABLE) == 1
                   ? 0
                   : 11;
    }
    unsigned long long eff = 0, prm = 0, inh = 0;
    if (mode[0] == 'a')
        return caps(&eff, &prm, &inh) && eff == CAP_BIT && prm == CAP_BIT && inh == CAP_BIT &&
                       prctl(PR_CAP_AMBIENT, PR_CAP_AMBIENT_IS_SET, 10, 0, 0) == 1 && getauxval(AT_SECURE) == 0
                   ? 0
                   : 13;
    if (mode[0] == 'f') {
        int valid = r == 1000 && e == 1000 && s == 1000 && caps(&eff, &prm, &inh) && eff == CAP_BIT && prm == CAP_BIT &&
                    inh == 0 && prctl(PR_CAP_AMBIENT, PR_CAP_AMBIENT_IS_SET, 10, 0, 0) == 0 &&
                    getauxval(AT_SECURE) == 1 && prctl(PR_GET_DUMPABLE) == 2;
        if (!valid) return 14;
        if (mode[7] == '-') return 0;
        if (unlink(FILECAP_PATH) != 0) return 15;
        execl("/proc/self/exe", "/proc/self/exe", "filecap-self", NULL);
        return 16;
    }
    if (mode[0] == 'm')
        return r == 1000 && e == 0 && s == 0 && getauxval(AT_SECURE) == 1 && prctl(PR_GET_DUMPABLE) == 2 ? 0 : 17;
    if (mode[0] == 'n')
        return r == 1000 && e == 1000 && s == 1000 && getauxval(AT_SECURE) == 0 && prctl(PR_GET_DUMPABLE) == 1
                   ? 0
                   : 18;
    if (mode[0] == 'g') {
        gid_t expected = mode[1] == '0' ? 1000 : 0;
        return gr == 1000 && ge == expected && gs == expected && getauxval(AT_SECURE) == (expected == 0) ? 0 : 19;
    }
    return r == 1000 && e == 0 && s == 0 && getauxval(AT_UID) == 1000 && getauxval(AT_EUID) == 0 &&
                   getauxval(AT_SECURE) == 1 && prctl(PR_GET_DUMPABLE) == 2
               ? 0
               : 12;
}

static int copy_self(const char *path, mode_t mode) {
    int in = open("/proc/self/exe", O_RDONLY), out = open(path, O_WRONLY | O_CREAT | O_TRUNC, 0755);
    if (in < 0 || out < 0) return -1;
    char bytes[16384];
    ssize_t got;
    while ((got = read(in, bytes, sizeof bytes)) > 0) {
        char *p = bytes;
        while (got > 0) {
            ssize_t put = write(out, p, (size_t)got);
            if (put <= 0) return -1;
            p += put;
            got -= put;
        }
    }
    close(in);
    if (close(out) || chown(path, 0, 0) || chmod(path, mode)) return -1;
    return 0;
}

static int make_interpreted_image(const char *path, const char *interpreter) {
    if (copy_self(path, 0755) != 0) return 0;
    int fd = open(path, O_RDWR);
    Elf64_Ehdr header;
    if (fd < 0 || pread(fd, &header, sizeof header, 0) != sizeof header) return 0;
    for (unsigned index = 0; index < header.e_phnum; ++index) {
        Elf64_Phdr program;
        off_t offset = (off_t)header.e_phoff + (off_t)index * header.e_phentsize;
        if (pread(fd, &program, sizeof program, offset) != sizeof program || program.p_type != PT_NOTE) continue;
        off_t end = lseek(fd, 0, SEEK_END);
        size_t length = strlen(interpreter) + 1;
        if (end < 0 || write(fd, interpreter, length) != (ssize_t)length) break;
        program.p_type = PT_INTERP;
        program.p_offset = (Elf64_Off)end;
        program.p_filesz = program.p_memsz = length;
        int ok = pwrite(fd, &program, sizeof program, offset) == sizeof program;
        close(fd);
        return ok;
    }
    close(fd);
    return 0;
}

static int run_ambient(void) {
    pid_t child = fork();
    if (child == 0) {
        unsigned long long eff, prm, inh;
        if (!caps(&eff, &prm, &inh) || setcaps(eff, prm, CAP_BIT) || prctl(PR_SET_KEEPCAPS, 1, 2, 3, 4) ||
            setresgid(1000, 1000, 1000) || setresuid(1000, 1000, 1000) || setcaps(CAP_BIT, CAP_BIT, CAP_BIT) ||
            prctl(PR_CAP_AMBIENT, PR_CAP_AMBIENT_RAISE, 10, 0, 0))
            _exit(30);
        execl("/proc/self/exe", "/proc/self/exe", "ambient", NULL);
        _exit(31);
    }
    int status = 0;
    return child > 0 && waitpid(child, &status, 0) == child && WIFEXITED(status) && WEXITSTATUS(status) == 0;
}

static int securebits_rules(void) {
    pid_t child = fork();
    if (child == 0) {
        errno = 0;
        if (prctl(PR_SET_SECUREBITS, 0x100) != -1 || errno != EINVAL) _exit(40);
        if (prctl(PR_SET_SECUREBITS, 0x30) != 0) _exit(41);
        errno = 0;
        if (prctl(PR_SET_SECUREBITS, 0) != -1 || errno != EPERM) _exit(42);
        _exit(0);
    }
    int status = 0;
    return child > 0 && waitpid(child, &status, 0) == child && WIFEXITED(status) && WEXITSTATUS(status) == 0;
}

static int forced_filecap_rejected(const char *path) {
    pid_t child = fork();
    if (child == 0) {
        if (prctl(PR_CAPBSET_DROP, 10, 0, 0, 0) != 0) _exit(43);
        execl(path, path, "filecap-direct", NULL);
        _exit(errno == EPERM ? 0 : 44);
    }
    int status = 0;
    return child > 0 && waitpid(child, &status, 0) == child && WIFEXITED(status) && WEXITSTATUS(status) == 0;
}

static int exec_errno(const char *path, int expected) {
    pid_t child = fork();
    if (child == 0) {
        execl(path, path, "ordinary", NULL);
        _exit(errno == expected ? 0 : 50);
    }
    int status = 0;
    return child > 0 && waitpid(child, &status, 0) == child && WIFEXITED(status) && WEXITSTATUS(status) == 0;
}

static int run(const char *path, const char *mode, int saved_root) {
    pid_t child = fork();
    if (child == 0) {
        if (setresgid(1000, 1000, saved_root ? 0 : 1000) || setresuid(1000, 1000, saved_root ? 0 : 1000)) _exit(20);
        execl(path, path, mode, NULL);
        _exit(21);
    }
    int status = 0;
    return child > 0 && waitpid(child, &status, 0) == child && WIFEXITED(status) && WEXITSTATUS(status) == 0;
}

static int run_ids(const char *path, const char *mode, uid_t ruid, uid_t euid, uid_t suid, gid_t rgid, gid_t egid,
                   gid_t sgid) {
    pid_t child = fork();
    if (child == 0) {
        if (setresgid(rgid, egid, sgid) || setresuid(ruid, euid, suid)) _exit(22);
        execl(path, path, mode, NULL);
        _exit(23);
    }
    int status = 0;
    return child > 0 && waitpid(child, &status, 0) == child && WIFEXITED(status) && WEXITSTATUS(status) == 0;
}

int main(int argc, char **argv) {
    if (argc == 2) return post(argv[1]);
    const char *setid = "/tmp/hl-exec-credentials-setid";
    const char *filecap = FILECAP_PATH;
    const char *malformed = MALFORMED_PATH;
    const char *unknown_flags = UNKNOWN_FLAGS_PATH;
    const char *bad_format = BAD_FORMAT_PATH;
    const char *bad_interpreter = BAD_INTERPRETER_PATH;
    const char *interpreted = INTERPRETED_PATH;
    const char *same_id = "/tmp/hl-exec-credentials-same-id";
    const char *setgid_plain = "/tmp/hl-exec-credentials-setgid-plain";
    const char *setgid_exec = "/tmp/hl-exec-credentials-setgid-exec";
    int prepared = copy_self(setid, 04755) == 0;
    uint32_t capability[5] = {0x02000001u, (uint32_t)CAP_BIT, 0, 0, 0};
    int file_directory =
        (mkdir(FILECAP_DIRECTORY, 0755) == 0 || errno == EEXIST) && chown(FILECAP_DIRECTORY, 1000, 1000) == 0;
    int file_prepared = file_directory && copy_self(filecap, 0755) == 0 &&
                        setxattr(filecap, "security.capability", capability, sizeof capability, 0) == 0;
    unsigned char bad[3] = {1, 2, 3};
    int malformed_prepared =
        copy_self(malformed, 0755) == 0 && setxattr(malformed, "security.capability", bad, sizeof bad, 0) == 0;
    uint32_t unknown[5] = {0x02000003u, (uint32_t)CAP_BIT, 0, 0, 0};
    int unknown_prepared = copy_self(unknown_flags, 0755) == 0 &&
                           setxattr(unknown_flags, "security.capability", unknown, sizeof unknown, 0) == 0;
    int bad_fd = open(bad_format, O_WRONLY | O_CREAT | O_TRUNC, 0755);
    int bad_format_prepared = bad_fd >= 0 && write(bad_fd, "not-elf", 7) == 7 && close(bad_fd) == 0 &&
                              setxattr(bad_format, "security.capability", bad, sizeof bad, 0) == 0;
    int bad_interpreter_fd = open(bad_interpreter, O_WRONLY | O_CREAT | O_TRUNC, 0755);
    int bad_interpreter_prepared =
        bad_interpreter_fd >= 0 && write(bad_interpreter_fd, "not-elf", 7) == 7 && close(bad_interpreter_fd) == 0 &&
        setxattr(bad_interpreter, "security.capability", bad, sizeof bad, 0) == 0 &&
        make_interpreted_image(interpreted, bad_interpreter);
    int same_id_prepared = copy_self(same_id, 0755) == 0 && chown(same_id, 1000, 1000) == 0 && chmod(same_id, 04755) == 0;
    int setgid_plain_prepared = copy_self(setgid_plain, 0755) == 0 && chown(setgid_plain, 1000, 0) == 0 &&
                                chmod(setgid_plain, 02700) == 0;
    int setgid_exec_prepared = copy_self(setgid_exec, 0755) == 0 && chown(setgid_exec, 1000, 0) == 0 &&
                               chmod(setgid_exec, 02710) == 0;
    int ordinary = run("/proc/self/exe", "ordinary", 1);
    int elevated = prepared && run(setid, "setid", 0);
    int ambient = run_ambient();
    int forced = file_prepared && forced_filecap_rejected(filecap);
    int file_caps = file_prepared && run(filecap, "filecap", 0);
    int unknown_rejected = unknown_prepared && exec_errno(unknown_flags, EINVAL);
    int precedence = malformed_prepared && chmod(malformed, 0644) == 0 && exec_errno(malformed, EACCES);
    int malformed_rejected = precedence && chmod(malformed, 0755) == 0 && exec_errno(malformed, EINVAL);
    int securebits = securebits_rules();
    int format_precedence = bad_format_prepared && exec_errno(bad_format, ENOEXEC);
    int interpreter_precedence = bad_interpreter_prepared && exec_errno(interpreted, ELIBBAD);
    int secure_mismatch = run_ids("/proc/self/exe", "mismatch", 1000, 0, 0, 1000, 1000, 1000);
    int secure_same = same_id_prepared && run_ids(same_id, "new-same", 1000, 0, 0, 1000, 1000, 1000);
    int setgid_plain_ok =
        setgid_plain_prepared && run_ids(setgid_plain, "g0", 1000, 1000, 1000, 1000, 1000, 1000);
    int setgid_exec_ok = setgid_exec_prepared && run_ids(setgid_exec, "g1", 1000, 1000, 1000, 1000, 1000, 1000);
    unlink(setid);
    unlink(filecap);
    rmdir(FILECAP_DIRECTORY);
    unlink(malformed);
    unlink(unknown_flags);
    unlink(bad_format);
    unlink(bad_interpreter);
    unlink(interpreted);
    unlink(same_id);
    unlink(setgid_plain);
    unlink(setgid_exec);
    printf("exec-credentials ordinary=%d setid=%d ambient=%d filecap=%d forced=%d malformed=%d flags=%d "
           "precedence=%d securebits=%d format=%d interpreter=%d mismatch=%d same=%d setgid-plain=%d setgid-exec=%d\n",
           ordinary, elevated, ambient, file_caps, forced, malformed_rejected, unknown_rejected, precedence,
           securebits, format_precedence, interpreter_precedence, secure_mismatch, secure_same, setgid_plain_ok,
           setgid_exec_ok);
    if (!ordinary || !elevated || !ambient || !file_caps || !forced || !malformed_rejected || !unknown_rejected ||
        !precedence || !securebits)
        return 1;
    if (!format_precedence) return 60;
    if (!interpreter_precedence) return 61;
    if (!secure_mismatch) return 62;
    if (!secure_same) return 63;
    if (!setgid_plain_ok) return 64;
    if (!setgid_exec_ok) return 65;
    return 0;
}
