# Repository compatibility tests

`compat/` is the persistent application-level guest corpus shared by the Rust
engine runner and the retained C baseline runner. It is deliberately outside a
runtime crate: these tests cross the application, loader, execution, Linux ABI,
and host-process boundaries.

The checked smoke corpus is small enough to keep in every checkout. The imported
retained corpus and its full artifact matrix preserve the migration oracle while
the normalized inventory lets both runners execute exactly the same binaries.
