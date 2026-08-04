// mkstemp/mkdtemp/tmpfile: templates get unique names, correct perms, and tmpfile auto-unlinks.
#include <stdlib.h>
#include <unistd.h>
#include <string.h>
#include <stdio.h>
#include <fcntl.h>
#include <sys/stat.h>

int main(void) {
    char tmpl[64];
    snprintf(tmpl, sizeof tmpl, "/tmp/hl_ms_%d_XXXXXX", (int)getpid());
    int fd = mkstemp(tmpl);
    int created = fd >= 0;
    int no_x = strstr(tmpl, "XXXXXX") == NULL; // template filled in
    struct stat st;
    int perm_ok = created && fstat(fd, &st) == 0 && (st.st_mode & 0777) == 0600;
    int rw = created && write(fd, "abc", 3) == 3;
    if (created) { close(fd); unlink(tmpl); }

    char dtmpl[64];
    snprintf(dtmpl, sizeof dtmpl, "/tmp/hl_md_%d_XXXXXX", (int)getpid());
    char *dir = mkdtemp(dtmpl);
    struct stat ds;
    int dir_ok = dir != NULL && stat(dir, &ds) == 0 && S_ISDIR(ds.st_mode) &&
                 (ds.st_mode & 0777) == 0700;
    if (dir) rmdir(dir);

    // tmpfile: writable, readable back, and has no visible link (st_nlink 0 once open).
    FILE *tf = tmpfile();
    int tf_ok = 0;
    if (tf) {
        fputs("hello", tf);
        fflush(tf);
        rewind(tf);
        char buf[8] = {0};
        tf_ok = fread(buf, 1, 5, tf) == 5 && strcmp(buf, "hello") == 0;
        struct stat ts;
        if (fstat(fileno(tf), &ts) == 0) tf_ok = tf_ok && ts.st_nlink == 0;
        fclose(tf);
    }

    printf("mkstempt created=%d filled=%d perm=%d rw=%d dir=%d tmpfile=%d\n",
           created, no_x, perm_ok, rw, dir_ok, tf_ok);
    return 0;
}
