// Self-timed on-disk SQLite point-select bench (profiling-only, this worktree).
// Reproduces the "python sqlite3 20k point selects by pk" perf workload MINUS the python
// interpreter, so a native-arm vs dd-arm compare isolates the SQLite (VDBE + B-tree + fcntl/pread)
// DBT overhead from the interpreter overhead. Self-times ONLY the select loop (startup excluded),
// printing `KERNEL sqlselect <ns>` in the same shape microbench.c uses.
//
//   b_sqlselect <db-path> [nrows] [nsel] [cache_kib]
//
// Each iteration = one point-select by PRIMARY KEY in its own implicit transaction (prepare once,
// bind random pk, step, reset). With a small page cache and a large table, that is exactly the
// fcntl(F_RDLCK)/pread(page)/fcntl(F_UNLCK) hot mix the file-backed sqlite-select workload issues.
#include <sqlite3.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

static uint64_t now_ns(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (uint64_t)ts.tv_sec * 1000000000ull + (uint64_t)ts.tv_nsec;
}
static void must(sqlite3 *db, int rc, const char *what) {
    if (rc != SQLITE_OK && rc != SQLITE_DONE && rc != SQLITE_ROW) {
        fprintf(stderr, "%s: %s\n", what, sqlite3_errmsg(db));
        exit(2);
    }
}
static void exec(sqlite3 *db, const char *sql) {
    char *err = 0;
    if (sqlite3_exec(db, sql, 0, 0, &err) != SQLITE_OK) {
        fprintf(stderr, "exec %s: %s\n", sql, err);
        exit(2);
    }
}

int main(int argc, char **argv) {
    const char *path = argc > 1 ? argv[1] : "/tmp/dd_sqlselect.db";
    long nrows = argc > 2 ? atol(argv[2]) : 200000;
    long nsel = argc > 3 ? atol(argv[3]) : 20000;
    long cache_kib = argc > 4 ? atol(argv[4]) : 512; // small cache -> force real preads

    unlink(path);
    sqlite3 *db;
    if (sqlite3_open(path, &db) != SQLITE_OK) return 2;
    char pragma[128];
    // negative cache_size = KiB of memory; small on purpose so the table does NOT fully fit.
    snprintf(pragma, sizeof pragma, "PRAGMA cache_size=-%ld;", cache_kib);
    exec(db, pragma);
    exec(db, "PRAGMA journal_mode=DELETE;"); // classic rollback journal -> real fcntl locking path
    exec(db, "CREATE TABLE t(id INTEGER PRIMARY KEY, k INTEGER, v REAL, s TEXT);");

    // Load nrows in one txn.
    exec(db, "BEGIN;");
    sqlite3_stmt *ins;
    sqlite3_prepare_v2(db, "INSERT INTO t(id,k,v,s) VALUES(?,?,?,?)", -1, &ins, 0);
    unsigned seed = 12345;
    for (long i = 1; i <= nrows; i++) {
        seed = seed * 1103515245u + 12345u;
        sqlite3_bind_int64(ins, 1, i);
        sqlite3_bind_int(ins, 2, (int)(seed % 100000));
        sqlite3_bind_double(ins, 3, (seed >> 8) * 1.5);
        char b[32];
        snprintf(b, sizeof b, "row-%u-padding-%u", seed % 9973, seed % 131);
        sqlite3_bind_text(ins, 4, b, -1, SQLITE_TRANSIENT);
        sqlite3_step(ins);
        sqlite3_reset(ins);
    }
    sqlite3_finalize(ins);
    exec(db, "COMMIT;");

    // Reopen cold so the OS/sqlite page caches start empty (forces pread traffic on the selects).
    sqlite3_close(db);
    if (sqlite3_open(path, &db) != SQLITE_OK) return 2;
    exec(db, pragma);

    sqlite3_stmt *sel;
    sqlite3_prepare_v2(db, "SELECT k, v, s FROM t WHERE id = ?", -1, &sel, 0);

    // Warm one pass so translation/cache is primed but page cache still churns (small cache).
    long checksum = 0;
    unsigned rs = 999;
    // --- timed region: nsel point selects by primary key, each its own txn ---
    uint64_t t0 = now_ns();
    for (long i = 0; i < nsel; i++) {
        rs = rs * 1103515245u + 12345u;
        long id = 1 + (long)(rs % (unsigned)nrows);
        sqlite3_bind_int64(sel, 1, id);
        if (sqlite3_step(sel) == SQLITE_ROW) {
            checksum += sqlite3_column_int(sel, 0);
            checksum += (long)sqlite3_column_double(sel, 1);
            const unsigned char *s = sqlite3_column_text(sel, 2);
            if (s) checksum += s[0];
        }
        sqlite3_reset(sel);
    }
    uint64_t t1 = now_ns();
    printf("KERNEL sqlselect %llu\n", (unsigned long long)(t1 - t0));
    fprintf(stderr, "checksum=%ld nsel=%ld nrows=%ld\n", checksum, nsel, nrows);
    fflush(stdout);

    sqlite3_finalize(sel);
    sqlite3_close(db);
    unlink(path);
    (void)must;
    return 0;
}
