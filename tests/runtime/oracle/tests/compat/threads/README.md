# Thread compatibility suite

This pure-C suite transfers every file and every registered case from the former
\`ext_threads\` guest group. Source files are byte-identical to their legacy
counterparts; only the descriptive destination names remove the old prefix.

\`manifest.tsv\` is authoritative. Every source is compiled as a static Linux
program for AArch64 and x86-64, run by its matching production engine, and must
exit with the declared status. Standard output must byte-match the checked-in
golden and must be identical across engines. There is no native oracle.

The suite covers creation, joining, detaching, attributes, scheduling, mutexes,
condition variables, rwlocks, TLS, once, cancellation, barriers, spinlocks,
named and unnamed semaphores, and C11 atomic operations. Timing-sensitive and
contention-heavy cases must also survive repeated execution; a single passing
run is not sufficient validation.
