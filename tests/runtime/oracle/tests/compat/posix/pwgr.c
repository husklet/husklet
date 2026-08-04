// getpwuid_r / getgrgid_r reentrant lookups for root (uid/gid 0) with buffer sizing via sysconf.
#include <pwd.h>
#include <grp.h>
#include <unistd.h>
#include <string.h>
#include <stdio.h>
#include <stdlib.h>

int main(void) {
    long pwsz = sysconf(_SC_GETPW_R_SIZE_MAX);
    if (pwsz <= 0) pwsz = 1024;
    char *buf = malloc(pwsz);
    struct passwd pw, *pwres = NULL;
    int rc = getpwuid_r(0, &pw, buf, pwsz, &pwres);
    // root must exist with uid 0; name is "root" on a standard system.
    int pw_ok = rc == 0 && pwres != NULL && pwres->pw_uid == 0 &&
                strcmp(pwres->pw_name, "root") == 0;
    free(buf);

    long grsz = sysconf(_SC_GETGR_R_SIZE_MAX);
    if (grsz <= 0) grsz = 1024;
    char *gbuf = malloc(grsz);
    struct group gr, *grres = NULL;
    int grc = getgrgid_r(0, &gr, gbuf, grsz, &grres);
    int gr_ok = grc == 0 && grres != NULL && grres->gr_gid == 0;
    free(gbuf);

    // A tiny buffer must fail with ERANGE, not corrupt memory.
    char tiny[1];
    struct passwd pw2, *r2 = NULL;
    int rc2 = getpwuid_r(0, &pw2, tiny, sizeof tiny, &r2);
    int erange = rc2 != 0 && r2 == NULL;

    printf("pwgr pw=%d gr=%d erange=%d\n", pw_ok, gr_ok, erange);
    return 0;
}
