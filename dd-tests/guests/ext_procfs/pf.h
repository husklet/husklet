// Shared helpers for the /proc /sys /dev content-conformance fixtures. Read a whole pseudo-file into a
// caller buffer (returns byte count or -1); tiny substring/line helpers. No libc surprises: plain read(2).
#ifndef PF_H
#define PF_H
#include <fcntl.h>
#include <stddef.h>
#include <string.h>
#include <unistd.h>

static int pf_read(const char *path, char *buf, int cap) {
    int fd = open(path, O_RDONLY);
    if (fd < 0) return -1;
    int o = 0, r;
    while (o < cap - 1 && (r = (int)read(fd, buf + o, (size_t)(cap - 1 - o))) > 0) o += r;
    close(fd);
    buf[o] = 0;
    return o;
}
// whole-word/substring presence
static int pf_has(const char *hay, const char *needle) { return strstr(hay, needle) != 0; }
// value after "key" up to newline copied into out (returns 1 if found). key must include its own
// trailing separator style handling by caller; we match at line start.
static int pf_line_val(const char *hay, const char *key, char *out, int outcap) {
    size_t kl = strlen(key);
    for (const char *p = hay; p && *p;) {
        if (!strncmp(p, key, kl)) {
            const char *v = p + kl;
            while (*v == ' ' || *v == '\t') v++;
            int i = 0;
            while (v[i] && v[i] != '\n' && i < outcap - 1) { out[i] = v[i]; i++; }
            out[i] = 0;
            return 1;
        }
        const char *nl = strchr(p, '\n');
        p = nl ? nl + 1 : 0;
    }
    return 0;
}
#endif
