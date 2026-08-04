// Container filesystem-root confinement. The /proc/self/root magic symlink must resolve to "/" (the
// guest's own root), never a host path — a leaked host prefix is a containment/identity escape. We
// also confirm /proc/self/cwd and getcwd(2) agree and yield an absolute path. The verdict is
// normalized (leading-slash + agreement booleans, never the raw cwd path, which is launch-dependent),
// identical on a bare host and a correct engine; the memo records a prior host-path leak through
// /proc/self/fd, so this guards the /proc/self/root member of that class.
#define _GNU_SOURCE
#include <stdio.h>
#include <string.h>
#include <unistd.h>

int main(void) {
    char root[1024] = {0}, cwd[1024] = {0};
    long rn = readlink("/proc/self/root", root, sizeof root - 1);
    long cn = readlink("/proc/self/cwd", cwd, sizeof cwd - 1);

    int root_ok = (rn > 0) && (strcmp(root, "/") == 0);
    int cwd_abs = (cn > 0) && (cwd[0] == '/');

    // getcwd(2) must be absolute and agree with the /proc/self/cwd magic link
    char g[1024] = {0};
    int got = getcwd(g, sizeof g) != NULL;
    int cwd_agree = (cn > 0) && got && strcmp(cwd, g) == 0;

    printf("root_is_slash=%d cwd_abs=%d\n", root_ok, cwd_abs);
    printf("getcwd_ok=%d getcwd_abs=%d cwd_link_agrees=%d\n",
           got, got && g[0] == '/', cwd_agree);
    return 0;
}
