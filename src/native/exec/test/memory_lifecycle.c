#define _XOPEN_SOURCE 700
#define HL_NATIVE_ALLOCATION_IMPLEMENTATION
#include "allocation.h"
#include "support.h"

#if defined(__linux__)
#include <dirent.h>
#include <string.h>
#endif
#include <signal.h>
#include <stdio.h>
#include <sys/wait.h>
#include <unistd.h>

#define CHECK(value) do { if (!(value)) { fprintf(stderr, "memory_lifecycle:%d: %s\n", __LINE__, #value); return 1; } } while (0)

static hl_native_direct_authority authority(void) {
    return (hl_native_direct_authority){
        .abi = HL_NATIVE_ABI, .size = sizeof(hl_native_direct_authority),
        .permissions = HL_NATIVE_ACCESS_READ, .guest_first = 0x1000,
        .guest_last = 0x2000, .host_first = 0x100000,
        .mapping_incarnation = 7, .mapping_generation = 8,
        .instruction_generation = 9,
    };
}

static int create_failures(void) {
    size_t successful_calls = 0;
    for (size_t failure = 1; failure < 32; ++failure) {
        test_memory state = {0};
        hl_native_memory memory = test_services(&state);
        hl_native_config config = test_config(&memory, 0);
        hl_native_executor *executor = NULL;
        hl_test_allocation_reset(failure);
        hl_native_status status = hl_native_create(&config, &executor);
        if (status == HL_NATIVE_OK) {
            successful_calls = hl_test_allocation_calls();
            CHECK(executor != NULL);
            CHECK(hl_native_destroy(executor) == HL_NATIVE_OK);
            CHECK(state.reserve_calls == 1 && state.release_calls == 1);
            CHECK(hl_test_allocation_live() == 0);
            break;
        }
        CHECK(status == HL_NATIVE_MEMORY && executor == NULL);
        CHECK(state.release_calls == state.reserve_calls);
        CHECK(hl_test_allocation_live() == 0);
    }
    CHECK(successful_calls == 8);
    return 0;
}

static int token_failures(void) {
    test_memory state = {0};
    hl_native_memory memory = test_services(&state);
    hl_native_config config = test_config(&memory, 0);
    hl_native_executor *executor = NULL;
    hl_native_direct_token *direct = NULL;
    hl_native_interrupt_token *interrupt = NULL;
    hl_native_direct_authority value = authority();
    hl_test_allocation_reset(0);
    CHECK(hl_native_create(&config, &executor) == HL_NATIVE_OK);
    size_t base = hl_test_allocation_live();
    hl_test_allocation_reset(1);
    CHECK(hl_native_direct_register(executor, &value, &direct) == HL_NATIVE_MEMORY && direct == NULL);
    CHECK(hl_test_allocation_live() == base);
    hl_test_allocation_reset(0);
    CHECK(hl_native_direct_register(executor, &value, &direct) == HL_NATIVE_OK && direct != NULL);
    CHECK(hl_native_direct_unregister(executor, direct) == HL_NATIVE_OK);
    CHECK(hl_test_allocation_live() == base);
    hl_test_allocation_reset(1);
    CHECK(hl_native_interrupt_create(&interrupt) == HL_NATIVE_MEMORY && interrupt == NULL);
    CHECK(hl_test_allocation_live() == base);
    hl_test_allocation_reset(0);
    CHECK(hl_native_interrupt_create(&interrupt) == HL_NATIVE_OK && interrupt != NULL);
    hl_native_interrupt_destroy(interrupt);
    CHECK(hl_test_allocation_live() == base);
    CHECK(hl_native_before_fork(executor) == HL_NATIVE_OK);
    pid_t child = fork();
    CHECK(child >= 0);
    if (child == 0) {
        CHECK(hl_native_after_fork(executor, 1) == HL_NATIVE_OK);
        CHECK(hl_native_destroy(executor) == HL_NATIVE_OK);
        CHECK(state.reserve_calls == state.release_calls && hl_test_allocation_live() == 0);
        _exit(0);
    }
    int status = 0;
    CHECK(waitpid(child, &status, 0) == child && WIFEXITED(status) && WEXITSTATUS(status) == 0);
    CHECK(hl_native_after_fork(executor, 1) == HL_NATIVE_OK);
    CHECK(hl_native_destroy(executor) == HL_NATIVE_OK);
    CHECK(state.reserve_calls == state.release_calls && hl_test_allocation_live() == 0);
    return 0;
}

static int altstack_failures(void) {
#if defined(__linux__)
    hl_test_allocation_reset(1);
    CHECK(hl_native_fault_thread_attach() == HL_NATIVE_MEMORY);
    CHECK(hl_test_allocation_live() == 0);
    for (unsigned iteration = 0; iteration < 32; ++iteration) {
        hl_test_allocation_reset(0);
        CHECK(hl_native_fault_thread_attach() == HL_NATIVE_OK);
        CHECK(hl_test_allocation_live() == 1);
        CHECK(hl_native_fault_thread_detach() == HL_NATIVE_OK);
        CHECK(hl_test_allocation_live() == 0);
    }
#endif
    return 0;
}

static int mapping_failures(void) {
    test_memory state = {.corrupt_mapping = 1};
    hl_native_memory memory = test_services(&state);
    hl_native_config config = test_config(&memory, 0);
    hl_native_executor *executor = NULL;
    hl_test_allocation_reset(0);
    CHECK(hl_native_create(&config, &executor) == HL_NATIVE_PLATFORM);
    CHECK(executor == NULL && state.reserve_calls == 1 && state.release_calls == 1);
    CHECK(hl_test_allocation_live() == 0);

    state = (test_memory){.fail_repair = 1};
    memory = test_services(&state);
    config = test_config(&memory, 0);
    CHECK(hl_native_create(&config, &executor) == HL_NATIVE_OK);
    CHECK(hl_native_before_fork(executor) == HL_NATIVE_OK);
    CHECK(hl_native_after_fork(executor, 0) == HL_NATIVE_PLATFORM);
    CHECK(hl_native_destroy(executor) == HL_NATIVE_OK);
    CHECK(state.reserve_calls == 1 && state.release_calls == 1);
    CHECK(hl_test_allocation_live() == 0);
    return 0;
}

#if defined(__linux__)
typedef struct process_resources {
    long rss_pages;
    unsigned mappings;
    unsigned descriptors;
    unsigned tasks;
} process_resources;

static unsigned directory_entries(const char *path) {
    DIR *directory = opendir(path);
    if (directory == NULL) abort();
    struct dirent *entry;
    unsigned count = 0;
    while ((entry = readdir(directory)) != NULL)
        if (strcmp(entry->d_name, ".") != 0 && strcmp(entry->d_name, "..") != 0) count++;
    if (closedir(directory) != 0) abort();
    return count;
}

static process_resources resources(void) {
    process_resources result = {0};
    FILE *file = fopen("/proc/self/statm", "r");
    unsigned long ignored, resident;
    if (file == NULL || fscanf(file, "%lu %lu", &ignored, &resident) != 2 || fclose(file) != 0) abort();
    result.rss_pages = (long)resident;
    file = fopen("/proc/self/maps", "r");
    if (file == NULL) abort();
    char line[512];
    while (fgets(line, sizeof(line), file) != NULL) result.mappings++;
    if (fclose(file) != 0) abort();
    result.descriptors = directory_entries("/proc/self/fd");
    result.tasks = directory_entries("/proc/self/task");
    return result;
}

static int steady_resources(void) {
    if (getenv("HL_NATIVE_SKIP_RESOURCE_COUNTS") != NULL) return 0;
    process_resources before = {0};
    for (unsigned iteration = 0; iteration < 32; ++iteration) {
        if (iteration == 16) before = resources();
        test_memory state = {0};
        hl_native_memory memory = test_services(&state);
        hl_native_config config = test_config(&memory, iteration & 1 ? HL_NATIVE_DUAL_REQUIRED : 0);
        hl_native_executor *executor = NULL;
        hl_test_allocation_reset(0);
        CHECK(hl_native_create(&config, &executor) == HL_NATIVE_OK);
        CHECK(hl_native_before_fork(executor) == HL_NATIVE_OK);
        CHECK(hl_native_after_fork(executor, iteration & 1) == HL_NATIVE_OK);
        CHECK(hl_native_destroy(executor) == HL_NATIVE_OK);
        CHECK(state.reserve_calls == 1 && state.release_calls == 1);
        CHECK(hl_test_allocation_live() == 0);
    }
    process_resources after = resources();
    long page_size = sysconf(_SC_PAGESIZE);
    CHECK(page_size > 0);
    CHECK(after.mappings == before.mappings);
    CHECK(after.descriptors == before.descriptors);
    CHECK(after.tasks == before.tasks);
    CHECK(after.rss_pages <= before.rss_pages + (64L << 20) / page_size);
    fprintf(stderr, "native-resource: maps=%u fds=%u tasks=%u rss_delta_pages=%ld\n",
            after.mappings, after.descriptors, after.tasks, after.rss_pages - before.rss_pages);
    return 0;
}
#else
static int steady_resources(void) { return 0; }
#endif

int main(void) {
    CHECK(create_failures() == 0);
    CHECK(token_failures() == 0);
    CHECK(altstack_failures() == 0);
    CHECK(mapping_failures() == 0);
    CHECK(steady_resources() == 0);
    return 0;
}
