// hl/linux_abi -- container config parsing: the strict numeric trust boundary.
//
// HL is the actual runtime that executes untrusted guest images AND is reachable directly via the
// main()/`docker` CLI (bypassing the typed Rust binding), so it must NOT trust its config input.
// Every HL_* numeric value is re-validated here and a bad value FAILS LOUD: a clear message to stderr
// + nonzero exit, never a silent coercion to a privileged/wrong default. The classic footgun this
// kills: `atoi("oops") == 0`, which would silently run the container as uid 0 (root).
//
// Shared by both the linux container code (os/linux/container/state.c) and the darwin jail
// Keep this parser guest-OS-neutral even though Linux is now the only supported guest ABI.
#ifndef HL_LINUX_PARSE_H
#define HL_LINUX_PARSE_H
#include <stddef.h>

// Parse a base-10 unsigned integer in [lo, hi]. Rejects empty / non-numeric / trailing garbage /
// negative / overflow. On ANY violation: print "hl: invalid <name>..." to stderr and exit nonzero.
unsigned long long hl_parse_u64(const char *name, const char *value, unsigned long long low, unsigned long long high);

// Container uid/gid: a valid id (0..INT_MAX). Garbage MUST error -- never fall back to 0 (= root).
int hl_parse_id(const char *name, const char *value);

// A TCP/UDP port: 1..65535. Rejects 0 and >65535 (which atoi would wrap into a wrong u16).
unsigned hl_parse_port(const char *name, const char *value);

// Parse a port from the field s[0..end) -- 'end' points just past the last char (e.g. at ':'/','),
// or NULL for "to end of string". Used by the HOST:CONTAINER publish parsers (delimited tokens).
unsigned hl_parse_port_field(const char *name, const char *value, const char *end);
#endif
