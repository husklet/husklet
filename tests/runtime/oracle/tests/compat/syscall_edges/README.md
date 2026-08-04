# Syscall edge compatibility corpus

These 19 byte-preserved guests come from legacy `syscallbug`. The manifest records every original
registration, both production ISAs, exact Linux errno/verdict golden, rootfs dependency, and untrusted
sentry variant. Tests intentionally pass malformed pointers, flags, masks, and timeouts; their exact
errors and non-consumption guarantees are the behavior contract.
