#include <sqlite3.h>

#include <errno.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <time.h>
#include <unistd.h>

enum { ROWS = 20000, WRITE_BATCHES = 10, READ_SCANS = 50 };

static uint64_t monotonic_microseconds(void) {
    struct timespec time;
    if (clock_gettime(CLOCK_MONOTONIC, &time) != 0) {
        perror("clock_gettime");
        exit(2);
    }
    return (uint64_t)time.tv_sec * 1000000U + (uint64_t)time.tv_nsec / 1000U;
}

static void require_sqlite(int status, sqlite3 *database, const char *operation) {
    if (status != SQLITE_OK && status != SQLITE_DONE && status != SQLITE_ROW) {
        fprintf(stderr, "%s: %s\n", operation, sqlite3_errmsg(database));
        exit(2);
    }
}

static void execute(sqlite3 *database, const char *sql) {
    char *message = NULL;
    int status = sqlite3_exec(database, sql, NULL, NULL, &message);
    if (status != SQLITE_OK) {
        fprintf(stderr, "sqlite3_exec: %s\n", message == NULL ? sqlite3_errmsg(database) : message);
        sqlite3_free(message);
        exit(2);
    }
}

static int write_all(const char *buffer, size_t length) {
    while (length != 0) {
        ssize_t written = write(STDOUT_FILENO, buffer, length);
        if (written < 0 && errno == EINTR) {
            continue;
        }
        if (written <= 0) {
            return -1;
        }
        buffer += (size_t)written;
        length -= (size_t)written;
    }
    return 0;
}

int main(void) {
    sqlite3 *database = NULL;
    require_sqlite(sqlite3_open(":memory:", &database), database, "sqlite3_open");

    uint64_t write_started = monotonic_microseconds();
    execute(database, "CREATE TABLE values_(value INTEGER NOT NULL);");
    sqlite3_stmt *insert = NULL;
    require_sqlite(sqlite3_prepare_v2(database, "INSERT INTO values_ VALUES (?1)", -1, &insert, NULL), database,
                   "prepare insert");
    sqlite3_int64 written = 0;
    for (int batch = 0; batch < WRITE_BATCHES; ++batch) {
        execute(database, "BEGIN; DELETE FROM values_;");
        for (int value = 1; value <= ROWS; ++value) {
            require_sqlite(sqlite3_bind_int(insert, 1, value), database, "bind insert");
            require_sqlite(sqlite3_step(insert), database, "step insert");
            require_sqlite(sqlite3_reset(insert), database, "reset insert");
            ++written;
        }
        execute(database, "COMMIT;");
    }
    require_sqlite(sqlite3_finalize(insert), database, "finalize insert");
    uint64_t write = monotonic_microseconds() - write_started;

    uint64_t read_started = monotonic_microseconds();
    sqlite3_stmt *query = NULL;
    require_sqlite(
        sqlite3_prepare_v2(database, "SELECT count(*), sum(value), sum(value * value) FROM values_", -1, &query, NULL),
        database, "prepare query");
    sqlite3_int64 count = 0;
    sqlite3_int64 checksum = 0;
    sqlite3_int64 square_checksum = 0;
    for (int scan = 0; scan < READ_SCANS; ++scan) {
        require_sqlite(sqlite3_step(query), database, "step query");
        count = sqlite3_column_int64(query, 0);
        checksum = sqlite3_column_int64(query, 1);
        square_checksum = sqlite3_column_int64(query, 2);
        if (count != (sqlite3_int64)ROWS || checksum != INT64_C(200010000) ||
            square_checksum != INT64_C(2666866670000)) {
            fputs("aggregate proof changed\n", stderr);
            return 3;
        }
        require_sqlite(sqlite3_reset(query), database, "reset query");
    }
    require_sqlite(sqlite3_finalize(query), database, "finalize query");
    uint64_t read = monotonic_microseconds() - read_started;

    if (write <= 1 || read <= 1) {
        fputs("measured duration was not greater than one microsecond\n", stderr);
        return 3;
    }
    char frame[256];
    int length = snprintf(frame, sizeof(frame),
                          "META workload=sqlite layout=sqlite version=1\n"
                          "PHASE sqlite-write us=%llu ok=%lld\n"
                          "PHASE sqlite-read us=%llu ok=%lld:%lld:%lld\n",
                          (unsigned long long)write, (long long)written, (unsigned long long)read, (long long)count,
                          (long long)checksum, (long long)square_checksum);
    if (length < 0 || (size_t)length >= sizeof(frame) || write_all(frame, (size_t)length) != 0) {
        return 4;
    }
    require_sqlite(sqlite3_close(database), database, "sqlite3_close");
    return 0;
}
