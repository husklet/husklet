// go-cgo-sigurg regression guest (regress.rs: "go-cgo-sigurg").
//
// An externally-linked / cgo (runtime.iscgo==1) aarch64 Go binary that forces heavy goroutine stack
// growth + GC while running tight call-free loops — exactly the workload Go's scheduler async-preempts
// with SIGURG. dd must either deliver that SIGURG correctly into JIT-translated code or (the interim
// fix, os/linux/signal.c g_go_iscgo) suppress it for the iscgo class, equivalent to
// GODEBUG=asyncpreemptoff=1. Pre-fix the delivery corrupted the preempted thread -> SIGSEGV/SIGBUS
// mid-run (empty stdout); post-fix the run completes and prints the golden line.
//
// 64 goroutines; goroutine g contributes exactly g, so the total is sum(0..63) = 2016:
//   "OK stackgrow total= 2016"
//
// Build (on linux/arm64, no cross-compile):
//   CGO_ENABLED=1 go build -trimpath -ldflags='-linkmode external -extldflags -static' \
//     -o go_cgo_stackgrow_arm go_cgo_stackgrow.go
// Install at <poc>/guests/arm/go_cgo_stackgrow_arm (see dd-tests/src/harness/provision/mod.rs resolve()).
package main

/*
#include <stdint.h>
// A real C-side leaf so the binary is genuinely cgo (runtime.iscgo==1) and threads bounce through
// cgocall/cgoSigtramp — the class whose SIGURG handling dd special-cases.
static uint64_t cwork(uint64_t n) {
	uint64_t s = 0;
	for (uint64_t i = 0; i < n; i++) s += i ^ (s << 1);
	return s;
}
*/
import "C"

import (
	"fmt"
	"runtime"
	"sync"
)

// Deep no-inline recursion with a fat frame: each level burns ~256B of stack, so 200 levels from a
// fresh 8KiB goroutine stack forces repeated runtime.morestack stack copies (the "stackgrow" part).
//
//go:noinline
func grow(depth, g int, buf [192]byte) int {
	if depth == 0 {
		return g + int(buf[0])
	}
	var b [192]byte
	b[0] = buf[0]
	return grow(depth-1, g, b)
}

// Tight call-free loop: the scheduler cannot cooperatively preempt it at a safepoint, so sysmon
// resorts to async preemption — SIGURG at an arbitrary PC inside JIT-translated code.
//
//go:noinline
func spin(n int) int {
	s := 0
	for i := 0; i < n; i++ {
		s += i &^ (s << 1)
	}
	return s
}

func main() {
	const ng = 64
	var wg sync.WaitGroup
	res := make([]int64, ng)
	for g := 0; g < ng; g++ {
		wg.Add(1)
		go func(g int) {
			defer wg.Done()
			var buf [192]byte
			for r := 0; r < 40; r++ {
				if grow(200, g, buf) != g { // morestack copies while other Gs get SIGURG-preempted
					panic("grow mismatch")
				}
				_ = spin(300000)        // async-preempt (SIGURG) target
				_ = C.cwork(2000)       // cgo transition on this thread
				if r%13 == g%13 {
					runtime.GC() // GC preempt-all: stack shrink/scan on every goroutine
				}
			}
			res[g] = int64(g)
		}(g)
	}
	wg.Wait()
	var total int64
	for _, v := range res {
		total += v
	}
	fmt.Println("OK stackgrow total=", total)
}
