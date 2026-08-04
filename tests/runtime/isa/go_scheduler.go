// x86/go-static-goro fixture: static -no-pie amd64 Go guest exercising the runtime SCHEDULER —
// 64 goroutines × 500k iterations churning through channels, enough work to draw real async
// preemption (the #250 crash site: a low-rewritten LEAQ asyncPreempt(SB) broke findfunc during
// runtime.init). The total is scheduling-independent, so the golden "goro tot= 16119975488" is
// byte-exact vs a native aarch64 build of this same source.
package main

import "fmt"

func main() {
	const G = 64     // goroutines
	const N = 500000 // iterations each
	ch := make(chan int64)
	for g := 1; g <= G; g++ {
		go func(g int) {
			var s int64
			for i := 1; i <= N; i++ {
				s += int64((i ^ g) % 1009)
			}
			ch <- s
		}(g)
	}
	var tot int64
	for g := 0; g < G; g++ {
		tot += <-ch
	}
	fmt.Println("goro tot=", tot)
}
