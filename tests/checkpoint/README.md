# Checkpoint backend matrix

`backend-matrix.tsv` is the executable inventory of checkpoint routes. It distinguishes host ISA,
guest ISA, and backend because “ARM on x86” is not one generic translation arm:

| Host | Guest | Execution body | Checkpoint contract |
| --- | --- | --- | --- |
| x86-64 | x86-64 | same-ISA transliterator | two capture/restore cycles |
| x86-64 | AArch64 | cross-ISA interpreter | two capture/restore cycles |
| AArch64 | AArch64 | same-ISA transliterator | two capture/restore cycles |
| AArch64 | x86-64 | cross-ISA translator | two capture/restore cycles |
| x86-64 | native x86-64 | native-supervised | known pre-launch refusal |
| AArch64 | native AArch64 | native-supervised | uncovered: the suite is x86-only |

The x86 -> ARM -> x86 arm is separate. It runs three architecture-bound engine bundles and the
final guest through `tests/runtime/nested/chains-x86.yaml`; the nested runner verifies every bundle
member against its manifest and verifies that every layer loaded its manifest-bound native library.

The runner never builds. Supply settled artifacts from the exact commit being gated, run each one
once before treating it as evidence, and then invoke:

```text
HL_CHECKPOINT_LINUX_TEST_BINARY=/absolute/checkpoint_linux-test \
HL_NATIVE_SUPERVISED_TEST_BINARY=/absolute/native_supervised-test \
HL_TESTING_BINARY=/absolute/testing \
.github/scripts/checkpoint-backend-matrix.sh \
  tests/checkpoint/backend-matrix.tsv /absolute/new-receipt-directory
```

The receipt binds every result to the manifest hash, test-binary hash, stdout hash, stderr hash,
exit status, and an exact once-only proof line. Missing artifacts, duplicate/missing proof lines,
timeouts, test failures, applicable coverage gaps, and unknown requested lanes fail closed. Pass lane
IDs after the receipt directory for a bounded smoke run; the runner rejects IDs for another host.

Native-supervised is not described as a round trip because it cannot currently create an image.
The x86 test proves that checkpoint selection refuses before launch. The AArch64 row is an explicit
failing gap rather than a skip. When native image capture exists, replace those contracts with real
kill-and-restore tests; merely changing the expected text would leave the product behavior untested.
