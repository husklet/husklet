//! The dispatcher's signal poll must examine the signals that are pending, not all sixty-four.
//!
//! `run_guest()` asks `signal_deliverable_for_cpu()` twice per dispatcher crossing and a crossing is
//! one guest basic block, so on `x86_64` Linux (`naa0245`) it was 22.5% of the user cycles a guest
//! `fork+exec` spends -- the largest single line in that profile -- for a process with no signal
//! outstanding at any point. Walking `64..=1` pays two `seq_cst` pending loads per signal *number*
//! before `signal_deliverable()` rejects it on the pending bits it just loaded.
//!
//! This gate is a probe *count*, because the count is the defect: the predicate's answer was always
//! right, and a test that only checked the answer would pass on the walk it replaced. The fixture
//! drives `signal_deliverable_for_cpu` over a recording `signal_deliverable`, so it pins both -- the
//! number of signals examined, and that every pending state still gets the same verdict.

use std::{fs, path::PathBuf, process::Command};

const PROBE: &str = r#"
#include <stdint.h>
#include <stdio.h>
#include <string.h>

/* The fragment reads only these fields of struct cpu; the signal domain's real one carries the rest. */
struct cpu {
    volatile uint64_t tpending;
    volatile uint64_t tpending_hi;
    uint64_t sigmask;
};

static volatile uint64_t g_pending;
static volatile uint64_t g_pending_hi;

static uint64_t thread_pending_hi_load(const struct cpu *cpu) {
    return __atomic_load_n(&cpu->tpending_hi, __ATOMIC_SEQ_CST);
}

static unsigned examined;      /* signals signal_deliverable() was asked about */
static uint64_t seen;          /* which ones, 1<<signo (signal 64 -> bit 0 of `seen_hi`) */
static unsigned seen_hi;

/* Stands in for linux_abi/signal.c's predicate, keeping the two rejections the scan relies on:
 * a signal that is not pending is not deliverable, and a masked one is pending but not deliverable. */
static int signal_deliverable(const struct cpu *cpu, int signal) {
    ++examined;
    if (signal == 64) seen_hi = 1; else if (signal >= 1 && signal <= 63) seen |= UINT64_C(1) << signal;
    if (signal < 1 || signal > 64) return 0;
    uint64_t bit = signal == 64 ? UINT64_C(1) : UINT64_C(1) << signal;
    volatile uint64_t *word = signal == 64 ? &g_pending_hi : &g_pending;
    uint64_t thread = signal == 64 ? cpu->tpending_hi : cpu->tpending;
    if (((*word | thread) & bit) == 0) return 0;
    if (cpu->sigmask & (UINT64_C(1) << (signal - 1))) return 0;
    return 1;
}

#include "linux_abi/signal_scan.h"

static struct cpu cpu_state;

static void reset(void) {
    memset(&cpu_state, 0, sizeof cpu_state);
    g_pending = 0;
    g_pending_hi = 0;
    examined = 0;
    seen = 0;
    seen_hi = 0;
}

/* The state the engine is in for nearly every dispatcher crossing of nearly every guest. */
static int nothing_pending_examines_no_signal(void) {
    reset();
    if (signal_deliverable_for_cpu(&cpu_state) != 0) {
        fprintf(stderr, "an empty pending set reported a deliverable signal\n");
        return 1;
    }
    if (examined != 0) {
        fprintf(stderr, "examined %u signals with nothing pending, expected 0\n", examined);
        return 2;
    }
    return 0;
}

/* A blocked signal stays pending and is not deliverable, so this state cannot be answered by an
 * early-out on "is anything pending" alone -- the scan must still reach exactly the pending signal
 * and no other. A walk that gives up and enumerates 1..64 whenever any bit is set fails here. */
static int a_blocked_pending_signal_examines_only_itself(void) {
    reset();
    g_pending = UINT64_C(1) << 17;                    /* SIGCHLD pending  */
    cpu_state.sigmask = UINT64_C(1) << (17 - 1);      /* ... and blocked  */
    if (signal_deliverable_for_cpu(&cpu_state) != 0) {
        fprintf(stderr, "a blocked pending signal was reported deliverable\n");
        return 10;
    }
    if (examined != 1) {
        fprintf(stderr, "examined %u signals for one pending signal, expected 1\n", examined);
        return 11;
    }
    if (seen != (UINT64_C(1) << 17)) {
        fprintf(stderr, "examined the wrong signal: seen=%llx\n", (unsigned long long)seen);
        return 12;
    }
    return 0;
}

/* Two pending, the lower one deliverable: the scan descends, so it rejects the higher first and
 * then accepts -- both are examined and nothing else is. */
static int a_deliverable_signal_is_found_in_descending_order(void) {
    reset();
    g_pending = (UINT64_C(1) << 30) | (UINT64_C(1) << 2);
    cpu_state.sigmask = UINT64_C(1) << (30 - 1);
    if (signal_deliverable_for_cpu(&cpu_state) != 1) {
        fprintf(stderr, "an unblocked pending signal was not reported deliverable\n");
        return 20;
    }
    if (examined != 2 || seen != ((UINT64_C(1) << 30) | (UINT64_C(1) << 2))) {
        fprintf(stderr, "examined=%u seen=%llx, expected exactly signals 30 and 2\n", examined,
                (unsigned long long)seen);
        return 21;
    }
    return 0;
}

/* Signal 64 does not fit the low word's bit-per-signo convention and lives in its own words, so it
 * has to be reached by a separate arm. A thread-directed one must be found too. */
static int signal_sixty_four_is_reached_through_the_hi_words(void) {
    reset();
    g_pending_hi = 1;
    if (signal_deliverable_for_cpu(&cpu_state) != 1 || !seen_hi || examined != 1) {
        fprintf(stderr, "process-directed signal 64: examined=%u seen_hi=%u\n", examined, seen_hi);
        return 30;
    }
    reset();
    cpu_state.tpending_hi = 1;
    if (signal_deliverable_for_cpu(&cpu_state) != 1 || !seen_hi || examined != 1) {
        fprintf(stderr, "thread-directed signal 64: examined=%u seen_hi=%u\n", examined, seen_hi);
        return 31;
    }
    return 0;
}

/* Thread-directed pending signals are the other half of the union the scan reads. */
static int a_thread_directed_signal_is_scanned_too(void) {
    reset();
    cpu_state.tpending = UINT64_C(1) << 9;
    if (signal_deliverable_for_cpu(&cpu_state) != 1) {
        fprintf(stderr, "a thread-directed pending signal was not reported deliverable\n");
        return 40;
    }
    if (examined != 1 || seen != (UINT64_C(1) << 9)) {
        fprintf(stderr, "examined=%u seen=%llx, expected exactly signal 9\n", examined,
                (unsigned long long)seen);
        return 41;
    }
    return 0;
}

int main(void) {
    int verdict = nothing_pending_examines_no_signal();
    if (verdict == 0) verdict = a_blocked_pending_signal_examines_only_itself();
    if (verdict == 0) verdict = a_deliverable_signal_is_found_in_descending_order();
    if (verdict == 0) verdict = signal_sixty_four_is_reached_through_the_hi_words();
    if (verdict == 0) verdict = a_thread_directed_signal_is_scanned_too();
    return verdict;
}
"#;

#[test]
fn the_dispatcher_poll_examines_only_the_signals_that_are_pending() {
    let package = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let native = package.join("src/native");
    let scratch = std::env::temp_dir().join(format!("hl-native-signal-scan-{}", std::process::id()));
    fs::create_dir_all(&scratch).expect("signal scan probe directory");
    let source = scratch.join("signal_scan.c");
    let executable = scratch.join("signal_scan");
    fs::write(&source, PROBE).expect("signal scan probe source");
    let compiler = std::env::var_os("CC").unwrap_or_else(|| "cc".into());
    let built = Command::new(&compiler)
        .args(["-std=c11", "-D_GNU_SOURCE"])
        .arg(format!("-I{}", native.display()))
        .arg(&source)
        .arg("-o")
        .arg(&executable)
        .status()
        .expect("compile signal scan probe");
    assert!(built.success(), "signal scan probe did not compile");
    let ran = Command::new(&executable).status().expect("run signal scan probe");
    assert!(ran.success(), "signal scan probe failed with {ran}");
    fs::remove_dir_all(scratch).expect("remove signal scan probe directory");
}
