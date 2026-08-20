use std::{fs, path::PathBuf, process::Command};

#[test]
fn mapping_sidecar_format_and_publication_contract_are_fail_closed() {
    let scratch = tempfile::tempdir().expect("create arena sidecar test directory");
    let source = scratch.path().join("arena_sidecar_test.c");
    let executable = scratch.path().join("arena_sidecar_test");
    let native = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/native");
    fs::write(
        &source,
        r#"
#include "engine/arena_sidecar.h"

#include <errno.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

#define CHECK(condition, code) do { if (!(condition)) return (code); } while (0)

static void put32(unsigned char *p, uint32_t v) {
    for (unsigned i = 0; i < 4; ++i) p[i] = (unsigned char)(v >> (8 * i));
}
static void put64(unsigned char *p, uint64_t v) {
    for (unsigned i = 0; i < 8; ++i) p[i] = (unsigned char)(v >> (8 * i));
}
static void seal(unsigned char *p, size_t n) {
    memset(p + 24, 0, 8);
    uint64_t h = UINT64_C(14695981039346656037);
    for (size_t i = 0; i < n; ++i) h = (h ^ p[i]) * UINT64_C(1099511628211);
    put64(p + 24, h);
}

static void fixture(hl_arena_mapping_sidecar *s, uint32_t isa) {
    memset(s, 0, sizeof(*s));
    s->guest_isa = isa;
    s->record_count = 2;
    s->granule = 4096;
    s->authority_nonce = 11;
    s->authority_identity = 12;
    s->generation = 13;
    s->records[0] = (hl_arena_mapping_source){1, UINT64_C(0x40000000), 8192, 0, 0,
                                                        HL_ARENA_MAPPING_ANONYMOUS,
                                                        HL_ARENA_PROTECTION_READ | HL_ARENA_PROTECTION_WRITE, 0, 0};
    s->records[1] = (hl_arena_mapping_source){2, UINT64_C(0x50000000), 12288, UINT64_C(0xfeed), 28672,
                                                        HL_ARENA_MAPPING_FILE,
                                                        HL_ARENA_PROTECTION_READ | HL_ARENA_PROTECTION_EXECUTE, 0, 0};
}

static hl_arena_sidecar_authority expected(const hl_arena_mapping_sidecar *s,
                                           hl_arena_expected_mapping *mappings) {
    for (uint32_t i = 0; i < s->record_count; ++i)
        mappings[i] = (hl_arena_expected_mapping){s->records[i].reservation_identity,
                                                  s->records[i].address, s->records[i].length};
    return (hl_arena_sidecar_authority){s->guest_isa, 0, s->granule, s->authority_nonce,
                                        s->authority_identity, s->generation, s->record_count, 0, mappings};
}

static int parses(const unsigned char *bytes, size_t size, const hl_arena_sidecar_authority *authority) {
    hl_arena_mapping_sidecar *parsed = calloc(1, sizeof(*parsed));
    if (parsed == NULL) return 0;
    int ok = hl_arena_mapping_sidecar_parse(bytes, size, authority, parsed) == 0;
    free(parsed);
    return ok;
}

static int round_trip(uint32_t isa) {
    hl_arena_mapping_sidecar *input = calloc(1, sizeof(*input));
    hl_arena_mapping_sidecar *output = calloc(1, sizeof(*output));
    unsigned char *bytes = malloc(HL_ARENA_SIDECAR_MAX_SIZE);
    CHECK(input != NULL && output != NULL && bytes != NULL, 1);
    fixture(input, isa);
    hl_arena_expected_mapping mappings[2];
    hl_arena_sidecar_authority authority = expected(input, mappings);
    size_t written = 0;
    CHECK(hl_arena_mapping_sidecar_encode(input, bytes, HL_ARENA_SIDECAR_MAX_SIZE, &written) == 0, 2);
    CHECK(written == HL_ARENA_SIDECAR_HEADER_SIZE + 2 * HL_ARENA_SIDECAR_RECORD_SIZE, 3);
    CHECK(memcmp(bytes, "HLARMAP1", 8) == 0, 4);
    static const unsigned char golden_header[80] = {
        72,76,65,82,77,65,80,49, 1,0,0,0, 80,0,0,0, 192,0,0,0,0,0,0,0,
        166,136,55,163,216,156,53,157, 1,0,0,0, 56,0,0,0, 2,0,0,0, 0,0,0,0,
        0,16,0,0,0,0,0,0, 11,0,0,0,0,0,0,0, 12,0,0,0,0,0,0,0, 13,0,0,0,0,0,0,0
    };
    static const unsigned char golden_records[112] = {
        1,0,0,0,0,0,0,0, 0,0,0,64,0,0,0,0, 0,32,0,0,0,0,0,0, 0,0,0,0,0,0,0,0,
        0,0,0,0,0,0,0,0, 1,0,0,0, 3,0,0,0, 0,0,0,0, 0,0,0,0,
        2,0,0,0,0,0,0,0, 0,0,0,80,0,0,0,0, 0,48,0,0,0,0,0,0, 237,254,0,0,0,0,0,0,
        0,112,0,0,0,0,0,0, 2,0,0,0, 5,0,0,0, 0,0,0,0, 0,0,0,0
    };
    if (isa == 1) CHECK(memcmp(bytes, golden_header, sizeof golden_header) == 0 &&
                        memcmp(bytes + sizeof golden_header, golden_records, sizeof golden_records) == 0, 8);
    CHECK(hl_arena_mapping_sidecar_parse(bytes, written, &authority, output) == 0, 5);
    CHECK(output->guest_isa == isa && output->record_count == 2 &&
          memcmp(output->records, input->records, 2 * sizeof(input->records[0])) == 0, 6);
    input->record_count = 0;
    authority = expected(input, mappings);
    CHECK(hl_arena_mapping_sidecar_encode(input, bytes, HL_ARENA_SIDECAR_MAX_SIZE, &written) == 0 &&
          written == HL_ARENA_SIDECAR_HEADER_SIZE &&
          hl_arena_mapping_sidecar_parse(bytes, written, &authority, output) == 0 && output->record_count == 0, 7);
    free(bytes); free(output); free(input);
    return 0;
}

static int malformed(void) {
    hl_arena_mapping_sidecar *sidecar = calloc(1, sizeof(*sidecar));
    unsigned char *original = malloc(HL_ARENA_SIDECAR_MAX_SIZE);
    unsigned char *bytes = malloc(HL_ARENA_SIDECAR_MAX_SIZE);
    CHECK(sidecar != NULL && original != NULL && bytes != NULL, 10);
    fixture(sidecar, 1);
    hl_arena_expected_mapping mappings[2];
    hl_arena_sidecar_authority authority = expected(sidecar, mappings);
    size_t size = 0;
    CHECK(hl_arena_mapping_sidecar_encode(sidecar, original, HL_ARENA_SIDECAR_MAX_SIZE, &size) == 0, 11);
    original[size] = 0;
    CHECK(!parses(original, size + 1, &authority), 48); /* appended bytes */
    hl_arena_mapping_sidecar *cleared = malloc(sizeof(*cleared));
    CHECK(cleared != NULL, 49); memset(cleared, 0xa5, sizeof(*cleared));
    CHECK(hl_arena_mapping_sidecar_parse(NULL, 0, &authority, cleared) == -1, 56);
    for (size_t i = 0; i < sizeof(*cleared); ++i) CHECK(((unsigned char *)cleared)[i] == 0, 57);
    memset(cleared, 0xa5, sizeof(*cleared));
    CHECK(hl_arena_mapping_sidecar_parse(original, 1, &authority, cleared) == -1, 58);
    for (size_t i = 0; i < sizeof(*cleared); ++i) CHECK(((unsigned char *)cleared)[i] == 0, 59);
    free(cleared);
    const size_t truncations[] = {0, 1, HL_ARENA_SIDECAR_HEADER_SIZE - 1, HL_ARENA_SIDECAR_HEADER_SIZE, 191};
    for (size_t i = 0; i < sizeof(truncations) / sizeof(truncations[0]); ++i)
        CHECK(!parses(original, truncations[i], &authority), 12);
    const size_t corruptions[] = {0, 8, 16, 24, 40, HL_ARENA_SIDECAR_HEADER_SIZE, 191};
    for (size_t i = 0; i < sizeof(corruptions) / sizeof(corruptions[0]); ++i) {
        memcpy(bytes, original, size); bytes[corruptions[i]] ^= 0x80;
        CHECK(!parses(bytes, size, &authority), 13);
    }
#define REJECT32(offset, value, code) do { memcpy(bytes, original, size); put32(bytes + (offset), (value)); \
    seal(bytes, size); CHECK(!parses(bytes, size, &authority), (code)); } while (0)
#define REJECT64(offset, value, code) do { memcpy(bytes, original, size); put64(bytes + (offset), (value)); \
    seal(bytes, size); CHECK(!parses(bytes, size, &authority), (code)); } while (0)
    REJECT32(8, 0, 14); REJECT32(8, 2, 15); REJECT32(12, 79, 16); REJECT32(36, 55, 17);
    REJECT32(40, UINT32_MAX, 18); REJECT32(44, 1, 19); REJECT64(16, UINT64_MAX, 20);
    REJECT32(32, 3, 21); REJECT64(48, 3000, 22); REJECT64(56, 0, 23);
    REJECT64(64, 0, 24); REJECT64(72, 0, 25);
    REJECT64(80 + 8, UINT64_MAX - 4095, 26);
    REJECT64(80 + 56 + 8, UINT64_C(0x40001000), 27); /* overlap */
    REJECT64(80 + 56, 1, 28);                         /* duplicate identity */
    REJECT32(80 + 40, 3, 29); REJECT32(80 + 44, 0, 30); REJECT32(80 + 44, 8, 31);
    REJECT32(80 + 48, 1, 32); REJECT32(80 + 52, 1, 33);
    REJECT64(80 + 24, 9, 34);                         /* anonymous identity */
    REJECT64(80 + 32, 4096, 35);                      /* anonymous offset */
    REJECT64(80 + 56 + 24, 0, 36);                   /* file identity */
    REJECT64(80 + 56 + 32, UINT64_MAX - 4095, 37);   /* file extent overflow */
    REJECT64(80 + 56 + 32, 1, 45);                   /* misaligned file offset */
    for (unsigned field = 0; field < 5; ++field) {
        hl_arena_sidecar_authority wrong = authority;
        if (field == 0) wrong.guest_isa = 2;
        if (field == 1) wrong.granule *= 2;
        if (field == 2) wrong.authority_nonce++;
        if (field == 3) wrong.authority_identity++;
        if (field == 4) wrong.generation++;
        CHECK(!parses(original, size, &wrong) && errno == EACCES, 38 + (int)field);
    }
    hl_arena_sidecar_authority wrong = authority;
    wrong.mapping_count = 1;
    CHECK(!parses(original, size, &wrong), 62);
    hl_arena_expected_mapping wrong_mappings[2] = {mappings[1], mappings[0]};
    wrong = authority; wrong.mappings = wrong_mappings;
    CHECK(!parses(original, size, &wrong) && errno == EACCES, 63);
    wrong_mappings[0] = mappings[0]; wrong_mappings[1] = mappings[1];
    wrong_mappings[0].reservation_identity++;
    CHECK(!parses(original, size, &wrong) && errno == EACCES, 64);
    wrong_mappings[0] = mappings[0]; wrong_mappings[0].address += 4096;
    CHECK(!parses(original, size, &wrong) && errno == EACCES, 65);
    wrong_mappings[0] = mappings[0]; wrong_mappings[0].length += 4096;
    CHECK(!parses(original, size, &wrong) && errno == EACCES, 66);
    unsigned char guard[16]; memset(guard, 0xa5, sizeof guard); size_t written = 99;
    CHECK(hl_arena_mapping_sidecar_encode(sidecar, guard, sizeof guard, &written) == -1 &&
          errno == ENOSPC && written == 0 && guard[0] == 0xa5 && guard[15] == 0xa5, 43);
    CHECK(hl_arena_mapping_sidecar_encode(sidecar, NULL, 0, &written) == -1 &&
          errno == EINVAL && written == 0, 60);
    CHECK(hl_arena_mapping_sidecar_encode(sidecar, guard, sizeof guard, NULL) == -1 && errno == EINVAL, 61);
    sidecar->record_count = HL_ARENA_SIDECAR_MAX_RECORDS + 1;
    CHECK(hl_arena_mapping_sidecar_encode(sidecar, bytes, HL_ARENA_SIDECAR_MAX_SIZE, &written) == -1 &&
          errno == EINVAL, 44);
    CHECK(hl_arena_mapping_sidecar_size(HL_ARENA_SIDECAR_MAX_RECORDS, &written) == 0 &&
          written == HL_ARENA_SIDECAR_MAX_SIZE, 46);
    CHECK(hl_arena_mapping_sidecar_size(HL_ARENA_SIDECAR_MAX_RECORDS + 1, &written) == -1 &&
          errno == EINVAL, 47);
    fixture(sidecar, 1); written = 0;
    CHECK(hl_arena_mapping_sidecar_encode(sidecar, bytes,
          HL_ARENA_SIDECAR_HEADER_SIZE + 2 * HL_ARENA_SIDECAR_RECORD_SIZE, &written) == 0 && written == size, 67);
    CHECK(hl_arena_mapping_sidecar_encode(sidecar, bytes, size - 1, &written) == -1 &&
          errno == ENOSPC && written == 0, 68);
#define INVALID_ENCODE(change, code) do { fixture(sidecar, 1); change; written = 99; \
    CHECK(hl_arena_mapping_sidecar_encode(sidecar, bytes, HL_ARENA_SIDECAR_MAX_SIZE, &written) == -1 && \
          errno == EINVAL && written == 0, (code)); } while (0)
    INVALID_ENCODE(sidecar->records[1].reservation_identity = 1, 69);
    INVALID_ENCODE(sidecar->records[1].address = UINT64_C(0x40001000), 70);
    INVALID_ENCODE(sidecar->records[0].address = UINT64_MAX - 4095, 71);
    INVALID_ENCODE(sidecar->records[1].source_offset = UINT64_MAX - 4095, 72);
    INVALID_ENCODE(sidecar->records[0].source_identity = 1, 73);
    INVALID_ENCODE(sidecar->records[1].source_identity = 0, 74);
    INVALID_ENCODE(sidecar->records[0].protection = 0, 75);
    INVALID_ENCODE(sidecar->records[0].flags = 1, 76);
    INVALID_ENCODE(sidecar->records[0].reserved = 1, 77);
    INVALID_ENCODE(sidecar->granule = 3000, 78);
#undef INVALID_ENCODE

    fixture(sidecar, 2); sidecar->record_count = HL_ARENA_SIDECAR_MAX_RECORDS;
    hl_arena_expected_mapping *maximum_mappings = calloc(HL_ARENA_SIDECAR_MAX_RECORDS, sizeof(*maximum_mappings));
    hl_arena_mapping_sidecar *maximum_output = calloc(1, sizeof(*maximum_output));
    CHECK(maximum_mappings != NULL && maximum_output != NULL, 79);
    for (uint32_t i = 0; i < HL_ARENA_SIDECAR_MAX_RECORDS; ++i) {
        sidecar->records[i] = (hl_arena_mapping_source){i + 1, UINT64_C(0x10000000) + (uint64_t)i * 4096,
            4096, 0, 0, HL_ARENA_MAPPING_ANONYMOUS, HL_ARENA_PROTECTION_READ, 0, 0};
    }
    hl_arena_sidecar_authority maximum_authority = expected(sidecar, maximum_mappings);
    CHECK(hl_arena_mapping_sidecar_encode(sidecar, bytes, HL_ARENA_SIDECAR_MAX_SIZE, &written) == 0 &&
          written == HL_ARENA_SIDECAR_MAX_SIZE, 80);
    CHECK(hl_arena_mapping_sidecar_parse(bytes, written, &maximum_authority, maximum_output) == 0 &&
          maximum_output->record_count == HL_ARENA_SIDECAR_MAX_RECORDS, 81);
    free(maximum_output); free(maximum_mappings);
    free(bytes); free(original); free(sidecar);
    return 0;
}

typedef struct publication_state {
    unsigned char staging[256], visible[256];
    size_t staged, visible_size;
    int fail_begin, fail_write, fail_commit, aborts;
    int begin_calls, write_calls, commit_calls, sequence;
} publication_state;
static int begin(void *opaque, uint64_t size) {
    publication_state *s = opaque; s->begin_calls++; s->sequence = s->sequence == 0 ? 1 : -100; s->staged = 0;
    return s->fail_begin || size > sizeof(s->staging) ? -1 : 0;
}
static int write_bytes(void *opaque, const void *bytes, size_t size) {
    publication_state *s = opaque; s->write_calls++; s->sequence = s->sequence == 1 ? 2 : -200;
    if (s->fail_write || size > sizeof(s->staging)) return -1;
    memcpy(s->staging, bytes, size); s->staged = size; return 0;
}
static int commit(void *opaque) {
    publication_state *s = opaque; s->commit_calls++; s->sequence = s->sequence == 2 ? 3 : -300;
    if (s->fail_commit) return -1; /* contract: still invisible on failure */
    memcpy(s->visible, s->staging, s->staged); s->visible_size = s->staged; return 0;
}
static void abort_publication(void *opaque) {
    publication_state *s = opaque; memset(s->staging, 0, sizeof(s->staging)); s->staged = 0; s->aborts++;
}
static int publication(void) {
    hl_arena_mapping_sidecar *sidecar = calloc(1, sizeof(*sidecar));
    unsigned char *scratch = malloc(HL_ARENA_SIDECAR_MAX_SIZE);
    CHECK(sidecar != NULL && scratch != NULL, 50); fixture(sidecar, 2); sidecar->record_count = 0;
    publication_state state = {0};
    hl_arena_sidecar_publication ops = {&state, begin, write_bytes, commit, abort_publication};
    CHECK(hl_arena_mapping_sidecar_publish(sidecar, scratch, HL_ARENA_SIDECAR_MAX_SIZE, &ops) == 0 &&
          state.visible_size == HL_ARENA_SIDECAR_HEADER_SIZE && state.aborts == 0 && state.begin_calls == 1 &&
          state.write_calls == 1 && state.commit_calls == 1 && state.sequence == 3, 51);
    unsigned char visible[256]; memcpy(visible, state.visible, sizeof visible); size_t visible_size = state.visible_size;
    sidecar->generation++; /* every failing attempt carries bytes distinct from the visible generation */
    state.fail_begin = 1; state.sequence = 0;
    CHECK(hl_arena_mapping_sidecar_publish(sidecar, scratch, HL_ARENA_SIDECAR_MAX_SIZE, &ops) == -1 &&
          errno == EIO && state.aborts == 0 && state.begin_calls == 2 && state.write_calls == 1 &&
          state.commit_calls == 1 && state.sequence == 1 && state.visible_size == visible_size &&
          memcmp(state.visible, visible, sizeof visible) == 0, 52);
    state.fail_begin = 0; state.fail_write = 1; state.sequence = 0;
    CHECK(hl_arena_mapping_sidecar_publish(sidecar, scratch, HL_ARENA_SIDECAR_MAX_SIZE, &ops) == -1 &&
          errno == EIO && state.aborts == 1 && state.begin_calls == 3 && state.write_calls == 2 &&
          state.commit_calls == 1 && state.sequence == 2 && state.staged == 0 &&
          state.visible_size == visible_size && memcmp(state.visible, visible, sizeof visible) == 0, 53);
    state.fail_write = 0; state.fail_commit = 1; state.sequence = 0;
    CHECK(hl_arena_mapping_sidecar_publish(sidecar, scratch, HL_ARENA_SIDECAR_MAX_SIZE, &ops) == -1 &&
          errno == EIO && state.aborts == 2 && state.begin_calls == 4 && state.write_calls == 3 &&
          state.commit_calls == 2 && state.sequence == 3 && state.staged == 0 &&
          state.visible_size == visible_size && memcmp(state.visible, visible, sizeof visible) == 0, 54);
    state.sequence = 0;
    CHECK(hl_arena_mapping_sidecar_publish(sidecar, scratch, 1, &ops) == -1 && state.aborts == 2 &&
          state.begin_calls == 4 && state.write_calls == 3 && state.commit_calls == 2 && state.sequence == 0, 55);
    free(scratch); free(sidecar); return 0;
}

int main(void) {
    int status = round_trip(1); if (status != 0) return status;
    status = round_trip(2); if (status != 0) return status;
    status = malformed(); if (status != 0) return status;
    return publication();
}
"#,
    )
    .expect("write arena sidecar probe");
    let output = Command::new(std::env::var_os("CC").unwrap_or_else(|| "cc".into()))
        .args(["-std=c11", "-Wall", "-Wextra", "-Werror"])
        .arg(format!("-I{}", native.display()))
        .arg(&source)
        .arg(native.join("engine/arena_sidecar.c"))
        .arg("-o")
        .arg(&executable)
        .output()
        .expect("compile arena sidecar probe");
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let status = Command::new(executable).status().expect("run arena sidecar probe");
    assert!(status.success(), "arena sidecar probe failed with {status}");
}
