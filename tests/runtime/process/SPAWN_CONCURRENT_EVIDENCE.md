# Concurrent spawn soak

`runtime/process/spawn-concurrent` was retained as broken after an intermittent
`pthread_create` `EAGAIN` was reported around one standalone run in ten. Twenty
clean target-runs were not enough to retire that record.

On 2026-08-24, the immutable release runner at commit `cec10dd1a` executed 50
repetitions on each of arm64 and amd64. All 100 uniquely identified target-runs
passed the native golden output and exit-status checks. The runner and engine
SHA-256 digests were respectively
`208c790984d9f2f2cf05e5ab867943500e6a9c1f1f322b243c5394632e8105da` and
`806e5777d8fde89012107b0f46082718c3c2984cfec10274aa7ca6b4ee22a16d`;
the exact ledger digest was
`4ebcff646e68ecb1d69a4514355d523caf94b320a65471e4b97488f22527fa44`.

Treating a paired arm64+amd64 repetition conservatively as one trial, 50 clean
trials leave `0.9^50`, about 0.515%, probability of missing a true independent
one-in-ten failure. The explicit soak remains available for future diagnosis;
the ordinary corpus now owns both targets as active compatibility checks.
