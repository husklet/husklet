# Repository refactoring plan

This directory is the planning authority for the repository-wide refactor. It describes work; it does
not authorize code, test, package, path, or brand changes by itself.

1. [`phase_1_audit/`](phase_1_audit/) — reduce dead weight and record what must be retained.
2. [`phase_2_tests/`](phase_2_tests/) — move behavioral ownership to the crate that owns the behavior.
3. [`phase_3_rebrand/`](phase_3_rebrand/) — rename the product to **husklet** after test ownership is stable.

The phases are ordered. Phase 2 must not be mixed with the rebrand: otherwise a failing test move cannot
be distinguished from a renamed package, binary, environment variable, or persisted path. Phase 3 starts
only after every moved test runs from its destination and the old aggregate target is empty of product
behavior.

## Non-negotiable rules

- Planning changes are documentation-only until a phase is explicitly authorized.
- Tests assert observable behavior, wire bytes, pixels, process results, files, or protocol traces. They
  do not pass by reading implementation source and finding a symbol or substring.
- C guest programs remain C fixtures where they exercise a C ABI. Test orchestration and assertions stay
  in Rust.
- Vendored/reference code is evidence, not a destination for project tests.
- A move is complete only when the destination owns its fixtures, invocation, CI gate, and failure output;
  deleting the old registration alone is not completion.
- Rebranding is a fresh-cutover plan, but cross-process and FFI names still change atomically.
