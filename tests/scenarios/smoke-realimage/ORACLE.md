# Real-image smoke workflow ownership

This folder replaces the former `workflows/smoke.rs` orchestration with three
folder-owned YAML cases. Each case preserves the former image, guest ISA,
`/bin/echo` process, expected marker, isolated network behavior, and bounded
container/image cleanup supplied by the repository scenario runner.

The former workflow always pulled into a fresh temporary cache. The unified
runner instead uses the selected platform cache and honors strict offline mode;
the guest-visible execution contract is unchanged while image acquisition is
now shared with every repository scenario.

Focused inventory evidence:

```text
cargo run -p testing --bin testing -- scenarios smoke-realimage --list
```

No retained C runtime mechanism changes in this ownership move. The retained C
engine remains a read-only behavioral oracle for the same guest programs.
