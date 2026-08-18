#ifndef HL_NAMESPACE_TRANSACTION_H
#define HL_NAMESPACE_TRANSACTION_H

#include <stdint.h>
#include <stdatomic.h>

typedef struct hl_host_services hl_host_services;

struct namespace_transaction_read {
    uint64_t sequence;
};

struct namespace_transaction_writer {
    _Atomic uint64_t *generation;
    _Atomic uint64_t *owner;
    uint64_t writer_generation;
    uint64_t writer_identity;
};

/* A stable namespace epoch makes pathname/object identity publication
 * linearizable. It does not legalize C data races: mutable payload fields read
 * inside the epoch (including owner metadata) must themselves be atomic or be
 * immutable after their atomic publication. */

static int namespace_transaction_init(const hl_host_services *host);
static int namespace_transaction_begin(void);
static void namespace_transaction_end(void);
static int namespace_transaction_read_begin(struct namespace_transaction_read *read);
static int namespace_transaction_read_validate(const struct namespace_transaction_read *read);
static int namespace_transaction_read_barrier(void);
static void namespace_transaction_fork_child(void);
static void namespace_transaction_fork_child_complete(void);
static int namespace_transaction_namespace(_Atomic uint64_t **generation, _Atomic uint64_t **owner);
static int namespace_transaction_writer(struct namespace_transaction_writer *writer);
static void namespace_transaction_poison(void);

#endif
