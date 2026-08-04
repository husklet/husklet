# Unusual runtime scenario oracle

These end-to-end cases preserve the executable contracts from
`tests/scenarios/fixtures/weird-core.yaml`, including IDs, images, commands,
targets, expected failures, environments, timeouts, exit status, and output.
The 24 embedded source heredocs are category-owned fixtures under `source/`;
the commands retain their original compile and execution steps.

The legacy scheduler held one outer `ProcessHeavy` permit for the category while
its inner case runner still admitted cases in parallel. The repository runner
accounts for resources per case. All 24 source-fixture cases invoke a guest
compiler and therefore declare `process_heavy`, as do the GHC compiler, JIT,
CPU-stress, package-install, and multithreaded workloads. Lightweight cases
without those mechanisms remain parallel:

```text
weird/beam-jit
weird/auxval-hwcap
weird/bpf-map-create
weird/cc-canary
weird/dotnet-ryujit
weird/eventfd
weird/gforth
weird/getrandom
weird/haskell-ghc
weird/inotify
weird/io-uring
weird/julia-jit
weird/jvm-c2-jit
weird/jvm-fib
weird/luajit-trace
weird/memfd-create
weird/node-primes
weird/openssl-speed
weird/prctl-name
weird/pthread-atomics
weird/ptrace-traceme
weird/pypy-compute
weird/ruby-yjit
weird/sched-affinity
weird/seccomp-filter
weird/self-modifying-rwx
weird/sigsegv-recover
weird/simd-probe
weird/smc-rewrite
weird/tcl
weird/timerfd
weird/tsc-counter
weird/userfaultfd
weird/v8-jit-in-jit
weird/vdso-clock
weird/xz-roundtrip
weird/zstd-roundtrip
```

This mapping retains mutual exclusion for resource-intensive work without
serializing lightweight probes that the legacy inner runner could overlap.
The four cases that install packages with APT (`gforth`, `tcl`, `xz-roundtrip`,
and `zstd-roundtrip`) additionally declare `network` and `disk_heavy`.

Exactly one legacy case is not migrated: `weird/static-nonpie-helloworld`. It
uses the `hello-world` image's configured entrypoint, while the repository
scenario executor currently refuses entrypoint execution because its materialized
image does not expose runtime entrypoint metadata. Replacing it with a guessed
command would weaken the contract, so the case remains documented as a runner
capability gap.

This is a representation and ownership migration only. It changes no engine
runtime behavior, so the retired C implementation was not used as an
implementation oracle and `/Users/x/dd/engine` was not modified.
