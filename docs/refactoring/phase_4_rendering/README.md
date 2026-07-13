# Phase 4: rendering correctness

Phase 4 owns the residual path from guest graphics APIs through shared IR, executors, composition and native
presentation. It follows the audit/test/rebrand planning phases but may supply evidence to all three. This is
a plan: code or test changes still require explicit implementation work.

## Active documents

- [`backlog.md`](backlog.md) — residual-only objectives and application acceptance matrix.
- [`chrome-fix-plan.md`](chrome-fix-plan.md) — retained multi-process Chrome engine/rendering investigation.
- [`validation.md`](validation.md) — code/history evidence used to remove completed ledger rows.
- [`research/shim-rust-architecture.md`](research/shim-rust-architecture.md) — maintained shim and rendering
  ownership boundaries.
- [`research/shim-gl-completeness.md`](research/shim-gl-completeness.md) and
  [`research/shim-cuda-completeness.md`](research/shim-cuda-completeness.md) — advertised API contracts.
- [`research/golden-harness.md`](research/golden-harness.md) — Metal pixel-golden procedure.
- [`reproduction/`](reproduction/) — Chromium, GTK4 and Vulkan workspace recipes.

Completed rows are deleted from the active backlog after validation. Old ledgers, promotion diaries,
branch-specific handoffs, benchmarks and screenshots are not maintained here; Git is their archive.

## Definition of done

A row closes only with observable Rust/C behavior at the owning boundary: ABI results/errors, negotiated wire
bytes, backend state, compositor pixels/resources, presentation timing or an unmodified application journey.
Required dependencies and devices fail preflight instead of turning a skipped journey into success. Tests that
only inspect source text are invalid for Phase 4.
