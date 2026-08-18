#ifndef HL_OFD_IDENTITY_H
#define HL_OFD_IDENTITY_H

#include <stdatomic.h>
#include <stddef.h>
#include <stdint.h>

#define HL_OFD_NAMESPACE_ABI UINT64_C(0x484c4f46444e5301)
#define HL_OFD_NAMESPACE_EMPTY UINT64_C(0)
#define HL_OFD_NAMESPACE_INITIALIZING UINT64_C(1)
#define HL_OFD_NAMESPACE_ACTIVE UINT64_C(2)

typedef struct hl_ofd_lineage {
    uint64_t high;
    uint64_t low;
} hl_ofd_lineage;

typedef struct hl_ofd_identity {
    hl_ofd_lineage lineage;
    uint64_t member;
    uint64_t sequence;
} hl_ofd_identity;

/* One instance belongs to one authenticated checkpoint generation. It must be
 * placed in topology-owned shared storage before any member can mint an OFD.
 * Forked members inherit the same mapping; restored members reopen that same
 * lineage object after authenticating each new checkpoint generation. */
typedef struct hl_ofd_namespace {
    _Atomic uint64_t state;
    uint64_t abi;
    uint64_t size;
    hl_ofd_lineage lineage;
    uint64_t generation_high;
    uint64_t generation_low;
    uint64_t generation_fence;
    _Atomic uint64_t next_member;
    _Atomic uint64_t next_sequence;
} hl_ofd_namespace;

typedef struct hl_ofd_member {
    hl_ofd_namespace *space;
    hl_ofd_lineage lineage;
    uint64_t ordinal;
} hl_ofd_member;

/* Authenticated, per-checkpoint-generation roll-forward metadata. Generation
 * rotates; lineage does not. Both high-water marks are exclusive and may only
 * increase across accepted generations. */
typedef struct hl_ofd_generation_binding {
    uint64_t generation_high;
    uint64_t generation_low;
    uint64_t fence;
    hl_ofd_lineage lineage;
    uint64_t next_member;
    uint64_t next_sequence;
} hl_ofd_generation_binding;

int hl_ofd_namespace_init(hl_ofd_namespace *space, size_t size, hl_ofd_lineage lineage,
                          int storage_is_zeroed);
int hl_ofd_member_bind(hl_ofd_member *member, hl_ofd_namespace *space, hl_ofd_lineage lineage,
                       uint64_t ordinal);
int hl_ofd_member_mint(hl_ofd_namespace *space, hl_ofd_member *member);
/* The broker may call this only with an authority-authenticated binding. C
 * enforces monotonic replay/high-water state; authentication keys stay Rust-only. */
int hl_ofd_namespace_admit_validated(hl_ofd_namespace *space, hl_ofd_generation_binding binding);
int hl_ofd_identity_mint(hl_ofd_member *member, hl_ofd_identity *identity);
int hl_ofd_identity_reattach(hl_ofd_member *member, hl_ofd_identity identity);
int hl_ofd_identity_equal(hl_ofd_identity first, hl_ofd_identity second);
int hl_ofd_identity_valid(hl_ofd_identity identity);

#if defined(HL_NATIVE_TEST_HOOKS)
int hl_ofd_identity_fixture(uint32_t scenario);
#endif

#endif
