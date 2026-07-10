# Golden-image regression harness (dd-display / Metal)

A **deterministic** check that rendering is correct — without a live Chrome, a window server, or any
flake. It replays captured **dd-gpu IR** streams (the exact bytes the GL shim forwards for one frame)
through the **real `MetalBackend`**, reads the composited surface back to a PNG, and pixel-diffs it
against a committed golden. Because it is pure replay → readback → compare, the same input always
produces the same pixels, so any rendering change (orientation/flip, blend, FBO composite, glyph
sampling) is caught the moment it changes output.

## Pieces

| Path | Role |
|------|------|
| `dd-display/tests/golden_image_regression.rs` | The harness: IR builders, a self-contained PNG decoder, the Metal replay+readback, and the diff. |
| `run_golden.sh` | One-command runner (`check` / `--update-goldens` / `--seed`). Uses the `mac` runner for Metal from the Linux dev host. |
| `target-chrome-codex/*.ir` | Captured/synthesized IR streams — one frame each. |
| `target-chrome-codex/golden/*.png` | Committed reference images. |
| `target-chrome-codex/oracle/*.png` | Optional external ground-truth screenshots (e.g. Weston). |
| `target-chrome-codex/rendered/`, `diff/` | Per-run output (git-ignored). `diff/` is written only on failure. |

## Running

```sh
./run_golden.sh                    # check: PASS/FAIL per case, non-zero exit on any regression
./run_golden.sh --update-goldens   # (re)write golden/*.png from this run — review the diff before committing!
./run_golden.sh --seed             # only regenerate the synthesized *.ir captures (no Metal needed)
```

Or drive cargo directly (Metal ⇒ macOS; on the Linux host prefix with `mac`):

```sh
mac bash -lc "cd <repo> && cargo test -p dd-display --test golden_image_regression golden_suite -- --nocapture --test-threads=1"
```

Tuning knobs (env vars):

- `DD_GOLDEN_TOL` — per-channel absolute tolerance (default `4`). Goldens produced on the same GPU match
  **bit-exactly** (maxΔ=0); the tolerance only absorbs cross-GPU rounding.
- `DD_GOLDEN_MAXPCT` — max fraction of pixels allowed to exceed `TOL` (default `0.0` = strict).
- `DD_ORACLE_STRICT=1` — make an oracle mismatch fail the suite (default: printed but informational).

A failing case prints `maxΔ`, the count/percent of differing pixels, and writes
`target-chrome-codex/diff/<name>.png` (over-tolerance pixels in red, sub-tolerance diff amplified in gray).

## The cases

Each pins a distinct part of the pipeline so an orientation/flip change can't regress silently:

| Case | Pins |
|------|------|
| `chrome-solid-quads` | Flat solid fills, per-quad color, clear color, quadrant placement. |
| `chrome-textured-glyph` | Texture upload + UV sampling + tint modulation (an asymmetric `F` glyph). |
| `chrome-offscreen-fbo` | Two-pass render: solid into an offscreen FBO, then sample it into the final surface. |
| `chrome-orientation` | A 4-color quadrant atlas — **any** flip/transpose/rotate permutes the quadrants. |
| `chrome-stream-ir-000` | **External-capture slot** (see below); skipped until a real capture lands. |

The four synthesized captures are asymmetric in both axes on purpose, so vertical flips, horizontal
flips, transposes, and 180° rotations all change the golden.

## Adding a golden

### A synthesized case (recommended for pinning a specific behavior)

1. Add a builder in the `cases` module of `golden_image_regression.rs` that returns a
   `dd_gpu::ir::encode_stream(&cmds)` byte stream (copy the closest existing builder). Texture id `1` is
   the injected render target — reference it as the final color attachment.
2. Register it in `registry()` with `build: Some(cases::your_case)`.
3. Regenerate the capture and seed the golden:
   ```sh
   ./run_golden.sh --seed              # writes target-chrome-codex/<name>.ir
   ./run_golden.sh --update-goldens    # renders + writes target-chrome-codex/golden/<name>.png
   ```
4. **Inspect the golden PNG** (open it / `Read` it) to confirm it's what you intend, then
   `./run_golden.sh` to confirm PASS, and commit `<name>.ir` + `golden/<name>.png`.

### A real Chrome capture

1. Capture one frame's IR from the GL shim via `DD_IR_DUMP` (the same bytes `dd-display selftest-shim-ir`
   consumes) and save it as `target-chrome-codex/<name>.ir`.
2. Register it with `build: None` (external slot) and the correct `w`/`h`; optionally set
   `oracle: Some("<file>.png")`.
3. `./run_golden.sh --update-goldens`, eyeball the golden, then `./run_golden.sh` and commit.

The pre-wired slot `chrome-stream-ir-000` (1280×720, oracle `chrome-weston.png`) is exactly this: drop
the `.ir` in and it activates.

## The oracle slot

To cross-check a case against an independent ground truth (e.g. a Weston screenshot of the same Chrome
frame), drop an 8-bit RGB/RGBA PNG at `target-chrome-codex/oracle/<name>` matching the case's render
size and point the case's `oracle:` field at it. By default the comparison is informational; set
`DD_ORACLE_STRICT=1` to gate on it. The harness's PNG decoder handles both our own stored-block goldens
and arbitrary zlib-compressed, filtered PNGs, so an externally-produced oracle just works.

## CI-like usage

The check is a normal cargo test, so a Metal-capable runner needs only:

```sh
cd <repo>
cargo test -p dd-display --test golden_image_regression golden_suite -- --test-threads=1
# exit code 0 = all goldens matched; non-zero = regression (artifacts in target-chrome-codex/diff/)
```

Notes:
- Single-threaded (`--test-threads=1`) keeps the per-case log readable and avoids concurrent Metal
  device churn.
- On a headless runner with **no** Metal device the suite prints `SKIP … no Metal device` and passes,
  so it never turns CI red for lack of a GPU — run it on a Metal-capable node to get real coverage.
- Only `*.ir`, `golden/`, and `oracle/` are committed; `rendered/` and `diff/` are git-ignored and
  regenerated each run (upload `diff/` as a CI artifact on failure).
