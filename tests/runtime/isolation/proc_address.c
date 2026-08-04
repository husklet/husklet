// Nothing under /proc may publish a memory address, or name a mapped file, that belongs to something
// other than this process. On an emulated host the engine's own address space sits alongside the guest's,
// so an unintercepted /proc file does not merely lose detail -- it hands the guest the engine's load
// address, its ASLR slide, and the absolute host path of its binary and libraries.
//
// /proc/self/{numa_maps,smaps_rollup,map_files} each did exactly that, and /proc/self/mem was the engine's
// own memory (readable AND writable through pwrite, which bypasses page protection). Every verdict here is
// a boolean derived from the guest's own /proc/self/maps, so the golden is identical on a bare host and on
// a correct engine, and each of those four leaks fails it.
#define _GNU_SOURCE
#include <ctype.h>
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#define VMA_MAX 512
static unsigned long long vlo[VMA_MAX], vhi[VMA_MAX];
static int nvma;
static unsigned long long span_lo, span_hi;
static char exe[4096];

// Own address space, from the one file whose confinement is already covered (pf-maps).
static void load_vmas(void) {
    FILE *f = fopen("/proc/self/maps", "r");
    char line[8192];
    if (!f) return;
    while (fgets(line, sizeof line, f) && nvma < VMA_MAX) {
        unsigned long long lo, hi;
        if (sscanf(line, "%llx-%llx", &lo, &hi) != 2) continue;
        vlo[nvma] = lo;
        vhi[nvma] = hi;
        if (nvma == 0 || lo < span_lo) span_lo = lo;
        if (nvma == 0 || hi > span_hi) span_hi = hi;
        nvma++;
    }
    fclose(f);
}

static int addr_is_ours(unsigned long long a) {
    for (int i = 0; i < nvma; i++)
        if (a >= vlo[i] && a < vhi[i]) return 1;
    return 0;
}

// A mapping pathname this process can account for: its own executable, or a kernel pseudo-region tag
// ([heap]/[stack]/[vdso]/[vvar]/[vsyscall]/anon). This guest maps no other file.
static int name_is_ours(const char *nm) {
    while (*nm == ' ')
        nm++;
    return !*nm || nm[0] == '[' || !strcmp(nm, exe) || !strncmp(nm, "anon_inode:", 11);
}

// Every hex token >= 0x100000 in `text` must fall inside one of our own VMAs. Below that threshold a token
// is a syscall argument or a byte count, not an address.
static int addrs_confined(const char *text) {
    int ok = 1;
    for (const char *p = text; *p;) {
        if (p[0] == '0' && p[1] == 'x' && isxdigit((unsigned char)p[2])) {
            char *end;
            unsigned long long v = strtoull(p + 2, &end, 16);
            if (v >= 0x100000ULL && !addr_is_ours(v)) ok = 0;
            p = end;
            continue;
        }
        p++;
    }
    return ok;
}

// A seq_file hands back one chunk per read(), so a single read truncates the larger /proc tables -- read
// to EOF or the comparison silently passes on the part it never saw.
static int slurp(const char *path, char *b, size_t n) {
    int fd = open(path, O_RDONLY);
    if (fd < 0) return -1;
    size_t got = 0;
    for (;;) {
        ssize_t r = read(fd, b + got, n - 1 - got);
        if (r <= 0) break;
        got += (size_t)r;
        if (got >= n - 1) break;
    }
    close(fd);
    b[got] = 0;
    return (int)got;
}

int main(void) {
    ssize_t el = readlink("/proc/self/exe", exe, sizeof exe - 1);
    exe[el > 0 ? el : 0] = 0;
    load_vmas();
    printf("vmas_ok=%d exe_ok=%d\n", nvma > 0, exe[0] == '/');

    static char b[1 << 20];

    // 1. numa_maps: one line per VMA. Its leading address must be one of ours, and any file= it names
    //    must be a file we mapped.
    int numa_addr = 1, numa_name = 1, numa_present = 0;
    if (slurp("/proc/self/numa_maps", b, sizeof b) >= 0) {
        numa_present = 1;
        for (char *line = strtok(b, "\n"); line; line = strtok(NULL, "\n")) {
            unsigned long long a = strtoull(line, NULL, 16);
            if (!addr_is_ours(a)) numa_addr = 0;
            char *fp = strstr(line, "file=");
            if (fp) {
                char nm[4096];
                snprintf(nm, sizeof nm, "%s", fp + 5);
                char *sp = strchr(nm, ' ');
                if (sp) *sp = 0;
                if (!name_is_ours(nm)) numa_name = 0;
            }
        }
    }
    printf("numa_present=%d numa_addrs_ours=%d numa_names_ours=%d\n", numa_present, numa_addr, numa_name);

    // 2. smaps_rollup: its header range must lie inside our own address span.
    int roll_ok = 1, roll_present = 0;
    if (slurp("/proc/self/smaps_rollup", b, sizeof b) >= 0) {
        unsigned long long lo, hi;
        roll_present = 1;
        roll_ok = sscanf(b, "%llx-%llx", &lo, &hi) == 2 && lo >= span_lo && hi <= span_hi;
    }
    printf("rollup_present=%d rollup_in_span=%d\n", roll_present, roll_ok);

    // 3. map_files/: "<start>-<end>" symlinks, one per file-backed VMA. Both bounds must be ours and the
    //    target must be a file we mapped.
    int mf_present = 0, mf_bounds = 1, mf_names = 1;
    DIR *d = opendir("/proc/self/map_files");
    if (d) {
        struct dirent *e;
        mf_present = 1;
        while ((e = readdir(d))) {
            if (e->d_name[0] == '.') continue;
            unsigned long long lo, hi;
            if (sscanf(e->d_name, "%llx-%llx", &lo, &hi) != 2 || !addr_is_ours(lo) || hi > span_hi) mf_bounds = 0;
            char p[4096], tgt[4096];
            snprintf(p, sizeof p, "/proc/self/map_files/%s", e->d_name);
            ssize_t r = readlink(p, tgt, sizeof tgt - 1);
            tgt[r > 0 ? r : 0] = 0;
            if (r <= 0 || !name_is_ours(tgt)) mf_names = 0;
        }
        closedir(d);
    }
    printf("map_files_present=%d map_files_bounds_ours=%d map_files_names_ours=%d\n", mf_present, mf_bounds, mf_names);

    // 4. syscall / stack / wchan: the kernel prints this task's own pc and sp here.
    int sc_ok = 1;
    if (slurp("/proc/self/syscall", b, sizeof b) >= 0) sc_ok = addrs_confined(b);
    printf("syscall_addrs_ours=%d\n", sc_ok);

    // 5. /proc/self/mem is this process's own memory. Reading an address no VMA of ours covers must fail;
    //    if it succeeds, the file is somebody else's address space.
    int mem_leaks = 0;
    int mfd = open("/proc/self/mem", O_RDONLY);
    if (mfd >= 0) {
        unsigned long long probe = 0x1000;
        while (probe < (1ULL << 46) && addr_is_ours(probe))
            probe <<= 1;
        char one;
        if (pread(mfd, &one, 1, (off_t)probe) == 1) mem_leaks = 1;
        close(mfd);
    }
    printf("mem_leaks_foreign=%d\n", mem_leaks);

    // 6. mountstats must describe the same namespace mountinfo does -- it fell through to the host and
    //    published the host block devices and mount paths while mountinfo next to it was confined.
    int ms_subset = 1, ms_present = 0;
    static char mi[1 << 18];
    if (slurp("/proc/self/mountinfo", mi, sizeof mi) >= 0 && slurp("/proc/self/mountstats", b, sizeof b) >= 0) {
        ms_present = 1;
        for (char *line = strtok(b, "\n"); line; line = strtok(NULL, "\n")) {
            char *on = strstr(line, " mounted on ");
            if (!on) continue;
            char mp[1024];
            snprintf(mp, sizeof mp, "%s", on + 12);
            char *w = strstr(mp, " with fstype");
            if (w) *w = 0;
            char needle[1030];
            snprintf(needle, sizeof needle, " %s ", mp);
            if (!strstr(mi, needle)) ms_subset = 0;
        }
    }
    printf("mountstats_present=%d mountstats_subset_of_mountinfo=%d\n", ms_present, ms_subset);
    return 0;
}
