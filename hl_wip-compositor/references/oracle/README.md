# references/oracle — retired hand-rolled Wayland server

The legacy hand-written protocol machine `hl-display/src/server.rs` (~4900 lines, plus its
`selftest.rs`) is **retired** here as a parity **oracle**, not shipped code. Smithay is now the ONE
Wayland path (`core/` + `handlers/`); this file records that when the merge lands, `server.rs` moves
here so its behavior can be diffed against the Smithay core during bring-up, then deleted once the
Smithay path is proven at parity.

First-party, tracked (distinct from the top-level gitignored `reference/` of external upstream
clones — OVERVIEW open decision D5).

Folds in from: `hl-display/src/server.rs`, `hl-display/src/selftest.rs`.
