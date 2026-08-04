# Docker network-list oracle audit

The authoritative compatibility oracle for this daemon API is Moby v24.0.9,
annotated tag object `227f2c673e96412a3c1eae3c047ec8b33231ece4`, peeled to source commit
`fca702de7f71362c8d103073c7e4a1d0a467fadd` (Docker API 1.43). The retained
C engine does not own Docker HTTP routing or network inventory, so no
`../engine` implementation participates in this lane.

## Sources studied

- `api/server/router/network/network_routes.go`, `getNetworksList`: parses the
  query, decodes and validates filters, combines cluster and local results,
  emits summaries for API 1.28 and newer, and guarantees an empty JSON array
  instead of `null`.
- `api/types/filters/parse.go`, `FromJSON`, `UnmarshalJSON`, `Get`,
  `ExactMatch`, `Match`, `MatchKVList`, `Validate`, and `WalkValues`: owns the
  current object-set and legacy array encodings. Object boolean values do not
  enable or disable terms; every object key is retained as a set member.
- `api/types/network/network.go`, `ValidateFilters`: admits exactly
  `dangling`, `driver`, `id`, `label`, `name`, `scope`, and `type`.
- `daemon/network/filter.go`, `FilterNetworks`, `filterNetworkByUse`, and
  `filterNetworkByType`: defines conjunction between keys, regular-expression
  alternatives for name and ID, conjunction for labels, exact driver and scope
  matching, dangling attachment behavior, and built-in/custom selection.
- `daemon/network.go`, `GetNetworks` and `buildNetworkResource`: owns local
  inventory projection. Detailed endpoint expansion is disabled for API 1.28
  and newer. The built-in network named `none` is exposed with driver `null`.

Moby owns network state in libnetwork and obtains one inventory snapshot before
filtering. Filtering performs no mutation, host call, or lock acquisition. The
list endpoint only projects owned response values; it does not extend endpoint
or network lifetimes. Errors from JSON decoding and accepted-key validation are
invalid-parameter responses (HTTP 400). Invalid `dangling` values and multiple
terms are also HTTP 400. Moby's invalid `type` error is not classified as an
invalid parameter and consequently reaches the HTTP mapper as HTTP 500.

## Husklet mapping and limits

`http/network/filter.rs::ListFilters` owns query decoding and matching.
`hl_container::Networks::list` owns the durable snapshot and sorts it by name,
which gives Husklet deterministic response order. `Network::from_summary` owns
the API 1.28+ projection and removes endpoint details without changing the
stored network. Raw-wire coverage exercises both supported JSON encodings,
filter conjunction and alternatives, regular expressions, dangling and type
selection, invalid-status behavior, and deterministic ordering.

Husklet currently implements local bridge and null-driver networks only. It has
no swarm/global network inventory or service attachments; unsupported scope and
driver terms therefore match no local networks. That is an explicit capability
gap, not a reason to alter the filtering rules.
