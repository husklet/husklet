# E/R/I cutover benchmark

`eri_matrix.py` runs the external C oracle (E), retained-C product path (R), and
integrated path (I) across Python, sqlite, and malloc. It automatically measures
E/E, R/R, I/I, E/R, E/I, and R/I. Each cell uses crossed `AB, BA, BA, AB`
ordering and therefore requires a multiple of four rounds.
Four crossed warmup pairs run before recorded samples in every cell, including
after resume.

The generated workloads deliberately run the combined sqlite and malloc phases
at divisor 2 and the Python loop for two million iterations.  Shorter smoke
workloads made identical-binary null arms vary by 5--10 percent on this host;
those rows are retained as rejected evidence, not used to relax qualification.

The direct Husklet product CLI routes through `ProductionFactory`.  It rejects
unknown and retired Rust backend requests and exposes the receipt below only
after that production selector constructs retained C for the requested ISA.

The generator now requires R and I to implement an executable receipt command:

```text
ENGINE --backend-receipt [--engine-option HL_EXECUTION_BACKEND=c]
```

The command must exit zero, write no stderr, and emit exactly one JSON value
whose schema is `husklet-engine-backend-v1`, whose backend is `retained-c`, and
whose `engine_sha256` matches the measured executable. R selects retained C
explicitly and I exercises the integrated product default; while both receipt
`retained-c`, that pair is a proven selector/default no-op control, not two
distinct engines. A future distinct integrated backend must receipt a distinct
name before the generator can label it as such. The matrix re-executes and verifies that receipt before every
campaign. If selection or receipt regresses, `eri_config.py` fails without
writing a configuration. A log line, accepted selector string, or identical
R/I binary is not backend evidence.

Once those receipts exist, generate a concrete configuration from separately
copied and smoke-tested artifacts. The generator hashes the adapter, every
engine and guest, and the complete Python rootfs:

```sh
tests/bench/eri_config.py \
  --external /absolute/copied/hl-engine-linux-aarch64 \
  --retained /absolute/copied/retained/hl-aarch64 \
  --integrated /absolute/copied/integrated/hl-aarch64 \
  --rootfs /absolute/python-rootfs \
  --python /absolute/python-rootfs/usr/local/bin/python3 \
  --combined /absolute/combined-plain-aarch64 \
  --combined-sqlite /absolute/combined-sqlite-aarch64 \
  --output /absolute/eri.json
```

`eri_adapter.py` runs each rootfs-aware engine directly and emits the matrix's
strict `PHASE` protocol. The external engine receives the guest path inside the
rootfs; the Husklet product receives the corresponding host-resolved path, so
the two loader coordinate contracts are explicit. Python's `python` phase uses
adapter wall time, covering interpreter startup, imports and work;
sqlite and malloc retain their in-guest `us`. A successful adapter writes no
stderr, so provider diagnostics cannot silently become guest-output identity.

Then run:

```sh
python3 tests/bench/eri_matrix.py \
  --config /absolute/path/to/eri.json \
  --results /var/tmp/eri-$(date +%Y%m%d-%H%M%S)
```

The results directory must not exist. To continue an interrupted campaign, pass
the same directory with `--resume`; its immutable manifest must match exactly.
Each completed invocation is appended and fsynced to `raw.jsonl`, so resume does
not replay it. Never edit that ledger.

The driver takes the measurement-intent lock before waiting for 120 seconds of
quiet, then takes `/var/tmp/husklet-box.lock` exclusively. It checks for at least
30 GiB free before starting. Quiet means no named benchmark/build process, no
holder of the box-lock inode found through `/proc/*/fd`, and one-minute load no
higher than `--max-load` (default 1.0). Output is exact after replacing only the numeric
`us=` field; phase sets and `ok=` values must also match across all three arms.

A null qualifies only when its paired center, crossed order strata, and temporal
strata are each within 1%; every pair is within 5%; and its measured floor is at
most 3%. Names listed in top-level `invariant_phases` use the tighter 1.5% floor.
Failure of any condition aborts without a performance verdict.

For each comparison and phase, the report uses the median paired ratio and the
larger of the two same-arm null floors. The conservative bound is
`ratio * (1 + null_floor)`. Every Python, sqlite, and malloc row must be at most
1.10; otherwise the process exits 2 and writes `FAIL` to `verdict.txt`.

## Adapter/no-op control

The command arrays are the executable adapter boundary: the driver appends the
same workload argv bytes to each. Before treating an E/R or E/I result as engine
cost, run a separate campaign config in which the two relevant labels invoke the
same backend through their respective adapters (for example, R and I both select
retained C). Keep the third label identical to either one. The ordinary null and
exact-output gates then apply unchanged; archive that campaign directory beside
the measured one. This is an executable no-op control, not a claim that the
example's placeholder selector names are current.
