#include "parse.h"

#include <errno.h>
#include <limits.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

unsigned long long hl_parse_u64(const char *name, const char *value, unsigned long long low, unsigned long long high) {
    if (!value || !*value || *value == '-') {
        fprintf(stderr, "hl: invalid %s=%s: not a number\n", name, value ? value : "");
        exit(2);
    }
    errno = 0;
    char *end = NULL;
    unsigned long long parsed = strtoull(value, &end, 10);
    if (errno != 0 || end == value || *end != '\0') {
        fprintf(stderr, "hl: invalid %s=%s: not a number\n", name, value);
        exit(2);
    }
    if (parsed < low || parsed > high) {
        fprintf(stderr, "hl: invalid %s=%s: out of range %llu..%llu\n", name, value, low, high);
        exit(2);
    }
    return parsed;
}

int hl_parse_id(const char *name, const char *value) {
    return (int)hl_parse_u64(name, value, 0, INT_MAX);
}

unsigned hl_parse_port(const char *name, const char *value) {
    return (unsigned)hl_parse_u64(name, value, 1, 65535);
}

unsigned hl_parse_port_field(const char *name, const char *value, const char *end) {
    char buffer[16];
    size_t size = end ? (size_t)(end - value) : strlen(value);
    if (size == 0 || size >= sizeof buffer) {
        fprintf(stderr, "hl: invalid %s: bad port field\n", name);
        exit(2);
    }
    memcpy(buffer, value, size);
    buffer[size] = '\0';
    return hl_parse_port(name, buffer);
}
