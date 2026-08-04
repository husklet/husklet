// fnmatch flags (PATHNAME, PERIOD, CASEFOLD) and glob() over a created directory tree.
#include <fnmatch.h>
#include <glob.h>
#include <fcntl.h>
#include <unistd.h>
#include <stdio.h>
#include <string.h>
#include <stdlib.h>
#include <sys/stat.h>

int main(void) {
    int m1 = fnmatch("a*c", "abc", 0) == 0;
    int m2 = fnmatch("a?c", "abc", 0) == 0;
    int m3 = fnmatch("[a-c]bc", "abc", 0) == 0;
    // FNM_PATHNAME: '*' does not match '/'.
    int m4 = fnmatch("a*c", "a/c", FNM_PATHNAME) == FNM_NOMATCH;
    // FNM_PERIOD: leading '.' not matched by '*'.
    int m5 = fnmatch("*x", ".x", FNM_PERIOD) == FNM_NOMATCH;
    int m6 = fnmatch("ABC", "abc", FNM_CASEFOLD) == 0;

    char dir[64];
    snprintf(dir, sizeof dir, "/tmp/hl_glob_%d", (int)getpid());
    mkdir(dir, 0755);
    const char *files[] = {"one.txt", "two.txt", "three.log", ".hidden"};
    for (int i = 0; i < 4; i++) {
        char p[128];
        snprintf(p, sizeof p, "%s/%s", dir, files[i]);
        int fd = open(p, O_CREAT | O_WRONLY, 0644);
        if (fd >= 0) close(fd);
    }
    char pat[128];
    snprintf(pat, sizeof pat, "%s/*.txt", dir);
    glob_t g = {0};
    int rc = glob(pat, 0, NULL, &g);
    // Two .txt files, sorted, hidden excluded.
    int glob_ok = rc == 0 && g.gl_pathc == 2;
    int sorted = 0;
    if (glob_ok) {
        const char *b0 = strrchr(g.gl_pathv[0], '/');
        const char *b1 = strrchr(g.gl_pathv[1], '/');
        sorted = b0 && b1 && strcmp(b0, "/one.txt") == 0 && strcmp(b1, "/two.txt") == 0;
    }
    globfree(&g);

    // Cleanup.
    for (int i = 0; i < 4; i++) {
        char p[128];
        snprintf(p, sizeof p, "%s/%s", dir, files[i]);
        unlink(p);
    }
    rmdir(dir);

    printf("fnmatchglob m1=%d m2=%d m3=%d m4=%d m5=%d m6=%d glob=%d sorted=%d\n",
           m1, m2, m3, m4, m5, m6, glob_ok, sorted);
    return 0;
}
