# E/R/I cutover benchmark

`eri_matrix.py` runs the external C oracle (E), retained-C product path (R), and
integrated path (I) across Python, sqlite, and malloc. It automatically measures
E/E, R/R, I/I, E/R, E/I, and R/I. Each cell uses crossed `AB, BA, BA, AB`
ordering and therefore requires a multiple of four rounds.

Copy `eri.example.json`, replace every absolute path and the backend selector
values with those printed by the current build, then run:

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
30 GiB free before starting. Output is exact after replacing only the numeric
`us=` field; phase sets and `ok=` values must also match across all three arms.

For each comparison and phase, the report uses the median paired ratio and the
larger of the two same-arm null floors. The conservative bound is
`ratio * (1 + null_floor)`. Every Python, sqlite, and malloc row must be at most
1.10; otherwise the process exits 2 and writes `FAIL` to `verdict.txt`.
