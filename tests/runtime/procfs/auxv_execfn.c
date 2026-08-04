// AT_EXECFN and auxv self-consistency. Linux sets AT_EXECFN to the pathname passed to execve (an
// absolute, openable path here), NOT argv[0] -- a fork+exec'd guest that re-execs via
// execve("/proc/self/exe", ...) with a relative argv[0] must still see an absolute AT_EXECFN (the
// sentry_exec_proc regression). All assertions print STABLE booleans (never the host-specific path or
// address) so the golden is identical on the native oracle and under the engine:
//   * AT_EXECFN present, absolute, strlen>1, and the named file opens;
//   * getauxval(AT_EXECFN) == the /proc/self/auxv value (the file mirrors the live vector);
//   * AT_PHDR/AT_PHENT/AT_PHNUM agree between getauxval and the file, AT_PHENT==sizeof(Elf64_Phdr),
//     and AT_PHDR points at REAL in-memory program headers (a PT_LOAD is found by walking them);
//   * AT_PAGESZ/AT_ENTRY consistent; the vector terminates with AT_NULL.
#define _GNU_SOURCE
#include <elf.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/auxv.h>
#include <unistd.h>

static unsigned long file_auxval(unsigned long want, int *present) {
    *present = 0;
    int fd = open("/proc/self/auxv", O_RDONLY);
    if (fd < 0) return 0;
    unsigned long pair[2], out = 0;
    for (;;) {
        ssize_t r = read(fd, pair, sizeof pair);
        if (r != (ssize_t)sizeof pair || pair[0] == AT_NULL) break;
        if (pair[0] == want) {
            out = pair[1];
            *present = 1;
        }
    }
    close(fd);
    return out;
}

int main(void) {
    int fp = 0, fpe = 0, fpn = 0, fpg = 0, fterm = 0;
    // Confirm the file terminates with AT_NULL and mirror a few live keys.
    {
        int fd = open("/proc/self/auxv", O_RDONLY);
        unsigned long pair[2];
        while (fd >= 0) {
            ssize_t r = read(fd, pair, sizeof pair);
            if (r != (ssize_t)sizeof pair) break;
            if (pair[0] == AT_NULL) {
                fterm = 1;
                break;
            }
        }
        if (fd >= 0) close(fd);
    }

    unsigned long execfn = getauxval(AT_EXECFN);
    unsigned long f_execfn = file_auxval(AT_EXECFN, &fp);
    int execfn_ok = execfn != 0 && fp && f_execfn == execfn;
    const char *name = (const char *)execfn;
    int execfn_abs = execfn_ok && name[0] == '/' && strlen(name) > 1;
    int execfn_open = 0;
    if (execfn_abs) {
        int e = open(name, O_RDONLY);
        execfn_open = e >= 0;
        if (e >= 0) close(e);
    }

    unsigned long phdr = getauxval(AT_PHDR), phent = getauxval(AT_PHENT), phnum = getauxval(AT_PHNUM);
    unsigned long f_phdr = file_auxval(AT_PHDR, &fpg), f_phent = file_auxval(AT_PHENT, &fpe),
                  f_phnum = file_auxval(AT_PHNUM, &fpn);
    int phdr_consistent = fpg && fpe && fpn && f_phdr == phdr && f_phent == phent && f_phnum == phnum;
    int phent_ok = phent == sizeof(Elf64_Phdr);
    // AT_PHDR must point at real in-memory program headers: walk phnum entries and require a PT_LOAD.
    int phdr_live = 0;
    if (phdr && phent == sizeof(Elf64_Phdr) && phnum > 0 && phnum < 256) {
        const Elf64_Phdr *ph = (const Elf64_Phdr *)phdr;
        for (unsigned long i = 0; i < phnum; i++)
            if (ph[i].p_type == PT_LOAD) {
                phdr_live = 1;
                break;
            }
    }

    int p_present = 0, e_present = 0;
    unsigned long f_pagesz = file_auxval(AT_PAGESZ, &p_present);
    unsigned long f_entry = file_auxval(AT_ENTRY, &e_present);
    int misc_ok = p_present && f_pagesz == getauxval(AT_PAGESZ) && e_present && f_entry == getauxval(AT_ENTRY);

    int ok = execfn_ok && execfn_abs && execfn_open && phdr_consistent && phent_ok && phdr_live && misc_ok &&
             fterm;
    printf("auxv_execfn ok=%d\n", ok);
    return 0;
}
