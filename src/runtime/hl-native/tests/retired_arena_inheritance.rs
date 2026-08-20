#![cfg(unix)]

use std::{fs, path::PathBuf, process::Command};

/// `retire_current` hands every fork child a hole where the retired code arena used to be, because the
/// child's fork hook unmaps every retired arena before it runs a single guest instruction and inheriting
/// an executable arena is expensive on macOS. Two properties have to hold for that to be safe, and both
/// are asserted here against the real `hl_arena_drop_child_inheritance`:
///
/// * the child must observe **no** retired arena -- a child that could still reach one would execute
///   code the parent is free to unmap under it;
/// * the parent must be completely undisturbed, because a peer guest thread parked mid-block inside the
///   retired arena resumes into it after the flush.
///
/// The control region is what makes the first assertion non-vacuous: it is mapped identically and is
/// **not** passed to the function, so the child inherits it. Without the call, the retired region would
/// be inherited exactly like the control and the probe fails.
#[test]
fn a_retired_arena_is_absent_in_a_fork_child_and_intact_in_the_parent() {
    let scratch = tempfile::tempdir().expect("create retired arena probe directory");
    let source = scratch.path().join("retired_arena_inheritance.c");
    let executable = scratch.path().join("retired_arena_inheritance");
    let native = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/native");
    fs::write(
        &source,
        r#"
#define _GNU_SOURCE
#define _DARWIN_C_SOURCE

#include "translator/arena.c"

#include <pthread.h>
#include <setjmp.h>
#include <signal.h>
#include <stdint.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/wait.h>
#include <unistd.h>

#define ARENA_SZ (1u << 20)
#define SENTINEL 0x5aa5c33cu

static sigjmp_buf g_fault;

static void on_fault(int signal) {
    (void)signal;
    siglongjmp(g_fault, 1);
}

/* 1 when `address` is readable in this process, 0 when it is a hole. */
static int readable(volatile uint32_t *address, uint32_t *value) {
    struct sigaction fault = {0};
    struct sigaction previous_bus;
    struct sigaction previous_segv;
    int ok;
    fault.sa_handler = on_fault;
    sigemptyset(&fault.sa_mask);
    sigaction(SIGBUS, &fault, &previous_bus);
    sigaction(SIGSEGV, &fault, &previous_segv);
    if (sigsetjmp(g_fault, 1) == 0) {
        *value = *address;
        ok = 1;
    } else
        ok = 0;
    sigaction(SIGBUS, &previous_bus, NULL);
    sigaction(SIGSEGV, &previous_segv, NULL);
    return ok;
}

static void *map_arena(void) {
#if defined(__APPLE__)
    /* The retired arenas this guards are MAP_JIT. Fall back to plain anonymous memory where the host
       refuses MAP_JIT to an unsigned probe: the inheritance contract is identical either way. */
    void *arena = mmap(NULL, ARENA_SZ, PROT_READ | PROT_WRITE | PROT_EXEC, MAP_PRIVATE | MAP_ANON | MAP_JIT, -1, 0);
    if (arena != MAP_FAILED) {
        pthread_jit_write_protect_np(0); /* MAP_JIT starts execute-only for this thread; the engine toggles too */
        return arena;
    }
#endif
    void *plain = mmap(NULL, ARENA_SZ, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANON, -1, 0);
    return plain == MAP_FAILED ? NULL : plain;
}

int main(void) {
    volatile uint32_t *retired = map_arena();
    volatile uint32_t *control = map_arena();
    /* MAP_SHARED so the child can report through it; it is never marked, so it survives the fork. */
    volatile uint32_t *report = mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_SHARED | MAP_ANON, -1, 0);
    pid_t child;
    int status = 0;
    uint32_t observed = 0;
    if (retired == NULL || control == NULL || report == MAP_FAILED) return 1;
    retired[0] = SENTINEL;
    control[0] = SENTINEL;
    memset((void *)report, 0, 4096);

    int dropped = hl_arena_drop_child_inheritance((void *)retired, ARENA_SZ);
#if defined(__APPLE__)
    if (!dropped) return 2; /* macOS is the host the retirement path is tuned for; it must succeed there. */
#else
    if (dropped) return 3; /* elsewhere the documented contract is a no-op, and the child inherits. */
#endif

    /* Parent-side: the drop changes inheritance only. Mapping and contents are untouched. */
    if (!readable(retired, &observed) || observed != SENTINEL) return 4;

    child = fork();
    if (child < 0) return 5;
    if (child == 0) {
        uint32_t value = 0;
        report[1] = (uint32_t)readable(retired, &value);
        report[2] = value;
        report[3] = (uint32_t)readable(control, &value);
        report[4] = value;
        report[0] = 1;
        _exit(0);
    }
    if (waitpid(child, &status, 0) != child || !WIFEXITED(status) || WEXITSTATUS(status) != 0) return 6;
    if (report[0] != 1) return 7;

    /* The control proves the child inherits an identically mapped region, so an absent retired arena is
       caused by the drop and not by fork, by MAP_JIT, or by the probe's own layout. */
    if (!report[3] || report[4] != SENTINEL) return 8;
#if defined(__APPLE__)
    if (report[1]) return 9; /* the child reached the retired arena -- the property the change rests on */
#else
    if (!report[1] || report[2] != SENTINEL) return 10;
#endif

    /* A peer parked mid-block in the retired arena resumes into it in THIS process after the fork. */
    if (!readable(retired, &observed) || observed != SENTINEL) return 11;
    if (!readable(control, &observed) || observed != SENTINEL) return 12;
    return 0;
}
"#,
    )
    .expect("write retired arena probe source");
    let compile = Command::new(std::env::var_os("CC").unwrap_or_else(|| "cc".into()))
        .args(["-std=c11", "-Wall", "-Wextra", "-Werror"])
        .arg(format!("-I{}", native.display()))
        .arg(format!("-I{}", native.join("include").display()))
        .arg(&source)
        .arg("-o")
        .arg(&executable)
        .output()
        .expect("retired arena probe compiler");
    assert!(compile.status.success(), "{}", String::from_utf8_lossy(&compile.stderr));
    let run = Command::new(&executable)
        .status()
        .expect("retired arena probe execution");
    assert!(run.success(), "retired arena probe failed with {run}");
}

/// The property above is only reachable from a unit boundary through `hl_arena_drop_child_inheritance`,
/// so this pins the one call site that puts it on the retirement path. `retire_current` is the single
/// definition both `engine/target/aarch64.c` and `engine/target/x86_64.c` unity-include, so the call is
/// ISA-independent by construction; the assertion below is what reddens if it is deleted.
#[test]
fn retiring_an_arena_drops_its_child_inheritance() {
    let cache = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/native/translator/cache.c");
    let text = fs::read_to_string(&cache).expect("read the translator code cache");
    let body = text
        .split_once("static int retire_current(void) {")
        .expect("retire_current is the arena retirement path")
        .1
        .split_once("\n}\n")
        .expect("retire_current has a body")
        .0;
    assert!(
        body.contains("hl_arena_drop_child_inheritance(g_cache, CACHE_SZ)"),
        "retire_current must drop the retired arena's child inheritance, or every later fork re-pays for it"
    );
    for target in ["engine/target/aarch64.c", "engine/target/x86_64.c"] {
        let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("src/native")
            .join(target);
        let text = fs::read_to_string(&source).expect("read a guest target translation unit");
        assert!(
            text.contains("translator/cache.c"),
            "{target} must unity-include the shared translator so retirement behaves identically on both ISAs"
        );
    }
}
