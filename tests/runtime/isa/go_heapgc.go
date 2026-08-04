// x86/go-static-heapgc fixture: static -no-pie amd64 Go guest with HEAVY GC churn — 200 rounds
// re-allocating 128 live 8KiB slices (~200MB total allocation) with explicit runtime.GC() sweeps,
// so the collector walks gcdata/gcbss masks and the lfstack under the engine's non-PIE high rebase
// (qemu-x86_64 cannot oracle this: its lfstack pointer-packing breaks at high heap addresses).
// The total is allocation-order-independent; the golden is byte-exact vs a native aarch64 build of
// this same source.
package main

import (
	"fmt"
	"runtime"
)

func main() {
	var total int64
	live := make([][]int64, 128)
	for round := 0; round < 200; round++ {
		for i := range live {
			b := make([]int64, 1024)
			for j := range b {
				b[j] = int64(round*i + j)
			}
			live[i] = b
		}
		var s int64
		for _, b := range live {
			s += b[0] + b[512] + b[1023]
		}
		total += s % 100003
		if round%32 == 0 {
			runtime.GC()
		}
	}
	fmt.Println("OK heapgc total=", total)
}
