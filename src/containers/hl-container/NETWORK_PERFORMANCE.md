# Network catalog performance audit

## Scope

This audit covers the durable Docker-style network catalog read by
`Networks::connect_many`. It does not change guest socket behavior or engine
network emulation.

## Retained oracle

The retained C engine was inspected read-only at:

- `../engine/src/core/target/aarch64.c`, launch-time `HL_NETNS` initialization;
- `../engine/src/core/target/x86_64.c`, launch-time `HL_NETNS` initialization;
- `../engine/src/linux_abi/container/state.c`, process-local network state;
- `../engine/src/linux_abi/container/netns.c`, `br_parse`, `nif_get`,
  `netns_tcp_bind_note`, `netns_tcp_listen_note`, and `netns_tcp_emit`;
- `../engine/pkgs/rust/src/network.rs`, typed launch identities, interfaces,
  and publication rules.

Those owners construct and retain process-local namespace, interface, socket,
and forwarding state. They do not own a durable Docker network catalog, peer
container lifecycle, or `/etc/hosts` regeneration. There is therefore no C
catalog loop to port. The matching Rust owners are `Networks` for durable
topology and mutation serialization, `NetworkStore` for snapshot persistence,
`Identity` for atomic identity-file replacement, and `NetworkConfig` for the
launch projection.

The operation lock keeps network mutation and peer refresh ordered. A failed
catalog commit rolls back previously replaced network records. Identity refresh
still happens only after all requested records commit, and failures still return
without pretending the refresh succeeded. Host and guest architecture do not
change these orchestration semantics; the architecture-specific behavior begins
after `NetworkConfig` reaches the engine.

## Measurement

`connect_inventory_bound` instruments `NetworkStore::list` for one attachment
to a network with 32 active peers. The baseline was executed from detached
revision `984dae648` with only the counter test added. Before the change, the operation performed 33
full catalog reads: one lookup plus one read for each peer refresh. After the
change it performs exactly two: one lookup and one post-commit snapshot shared by
all refreshes. The bound changes from `O(P * N)` catalog cloning to `O(N + P)`
lookups over one owned snapshot, where `P` is refreshed peers and `N` is networks.
With no active peer to refresh, both implementations perform only the lookup; the
snapshot is loaded lazily after the peer set is known to be nonempty.

The test is intentionally an operation-count measurement rather than a wall-time
threshold, so scheduler and filesystem variance cannot hide a regression.

Both the baseline and candidate were run with:

```sh
CARGO_TARGET_DIR=/Users/x/dd/husklet/target CFLAGS=-O1 \
  RUSTFLAGS='-D warnings' \
  nix --extra-experimental-features 'nix-command flakes' develop -c \
  cargo test -p hl-container connect_inventory_bound --locked -- --nocapture
```

The candidate run used the same detached revision with the catalog-snapshot
patch applied. Each run passed one focused test with 148 unit tests filtered out;
the package integration binaries also completed with their filters empty.
