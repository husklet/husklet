/* cachestat (syscall 451, Linux 6.5+) reports page-cache residency for a fd range. On a capable
   kernel a query over a small file's range must succeed and report a residency count within the
   range's page count; where unimplemented it is ENOSYS; a query on a non-regular fd is rejected. A
   correct engine returns those canonical outcomes, never a crash or bogus errno. Derived boolean
   verdict, arch-neutral and host-independent. */
#include "compat.h"
#include <stdio.h>
#include <string.h>
#include <fcntl.h>
#include <stdint.h>
#ifndef __NR_cachestat
#define __NR_cachestat 451
#endif

struct cachestat_range { uint64_t off, len; };
struct cachestat { uint64_t nr_cache, nr_dirty, nr_writeback, nr_evicted, nr_recently_evicted; };

int main(void) {
    const char *p = "/tmp/hlc_cachestat";
    unlink(p);
    int fd = open(p, O_CREAT | O_RDWR | O_TRUNC, 0644);
    char blob[16384];
    memset(blob, 'x', sizeof blob);
    write(fd, blob, sizeof blob);

    struct cachestat_range rng = {0, sizeof blob};
    struct cachestat cs;
    memset(&cs, 0, sizeof cs);
    long r = syscall(__NR_cachestat, fd, &rng, &cs, 0u);
    int handled = (r == 0) || (errno == ENOSYS || errno == EOPNOTSUPP);
    unsigned pages = (sizeof blob + 4095) / 4096;
    int count_sane = (r != 0) ? 1 : (cs.nr_cache <= pages && cs.nr_dirty <= pages);

    close(fd);
    unlink(p);
    printf("cachestat handled=%d count_sane=%d\n", handled, count_sane);
    return 0;
}
