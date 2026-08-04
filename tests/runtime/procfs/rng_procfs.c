// /proc/sys/kernel/random/ contract: `uuid` yields a FRESH valid type-4 UUID on every read (two reads
// DIFFER, both well-formed 8-4-4-4-12 with version nibble 4 and variant bits 10xx); `boot_id` is a valid
// UUID that is STABLE across reads within the boot (two reads SAME); `entropy_avail` and `poolsize` are
// plausible positive integers. uuid-differs + boot_id-stable is the key security pair. Only stable
// booleans printed -> native == engine byte-for-byte.
#define _GNU_SOURCE
#include <ctype.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static int read_line(const char *path, char *out, size_t cap) {
    FILE *f = fopen(path, "r");
    if (!f) return 0;
    int ok = fgets(out, (int)cap, f) != NULL;
    fclose(f);
    if (ok) out[strcspn(out, "\n")] = 0;
    return ok;
}

// Validate the canonical 36-char UUID form 8-4-4-4-12 with version 4 and variant 10xx.
static int uuid_valid(const char *s) {
    if (strlen(s) != 36) return 0;
    for (int i = 0; i < 36; i++) {
        if (i == 8 || i == 13 || i == 18 || i == 23) {
            if (s[i] != '-') return 0;
        } else if (!isxdigit((unsigned char)s[i])) {
            return 0;
        }
    }
    if (s[14] != '4') return 0; // version nibble
    char v = tolower((unsigned char)s[19]); // variant: 8,9,a,b
    if (!(v == '8' || v == '9' || v == 'a' || v == 'b')) return 0;
    return 1;
}

int main(void) {
    char u1[64] = {0}, u2[64] = {0}, b1[64] = {0}, b2[64] = {0}, ent[64] = {0}, pool[64] = {0};

    int gu1 = read_line("/proc/sys/kernel/random/uuid", u1, sizeof u1);
    int gu2 = read_line("/proc/sys/kernel/random/uuid", u2, sizeof u2);
    int uuid_fmt_ok = gu1 && gu2 && uuid_valid(u1) && uuid_valid(u2);
    int uuid_differ = gu1 && gu2 && strcmp(u1, u2) != 0;

    int gb1 = read_line("/proc/sys/kernel/random/boot_id", b1, sizeof b1);
    int gb2 = read_line("/proc/sys/kernel/random/boot_id", b2, sizeof b2);
    int boot_fmt_ok = gb1 && gb2 && uuid_valid(b1) && uuid_valid(b2);
    int boot_stable = gb1 && gb2 && strcmp(b1, b2) == 0;

    int ent_ok = read_line("/proc/sys/kernel/random/entropy_avail", ent, sizeof ent) && atoi(ent) > 0;
    int pool_ok = read_line("/proc/sys/kernel/random/poolsize", pool, sizeof pool) && atoi(pool) > 0;

    printf("uuid_fmt=%d uuid_differ=%d boot_fmt=%d boot_stable=%d entropy=%d poolsize=%d\n", uuid_fmt_ok,
           uuid_differ, boot_fmt_ok, boot_stable, ent_ok, pool_ok);
    return 0;
}
