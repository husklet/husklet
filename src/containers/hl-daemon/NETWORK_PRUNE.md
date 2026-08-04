# Network prune filters

Moby v24.0.9 is the API 1.43 behavior oracle for this boundary.

Audited sources and entry points:

- `api/server/router/network/network_routes.go`, `postNetworksPrune`: parses the
  query form, decodes the `filters` JSON object, calls `NetworksPrune`, and writes
  the report as HTTP 200 JSON.
- `daemon/prune.go`, `NetworksPrune`, `localNetworksPrune`,
  `getUntilFromPruneFilters`, and `matchLabels`: accepts only `label`, `label!`,
  and `until`; validates exactly one cutoff; prunes non-predefined networks with
  no endpoints; includes networks created exactly at the cutoff; requires all
  positive labels; and excludes a network only when all negative labels match.
- `api/types/filters/parse.go`, `Args.Validate` and `Args.MatchKVList`: validates
  the accepted filter-name set and defines the all-values label matching used by
  both positive and negated filters.

Husklet ownership maps HTTP decoding and response status to
`hl-daemon::api::http::network`, filter validation to
`network::prune::Filters`, durable topology and endpoint checks to
`hl-container::Networks`, and event publication to `hl-daemon::Events`.
This proves local-network filter compatibility only. Moby also serializes every
prune family behind a daemon-wide conflict guard, skips config-only networks, and
walks cluster networks through its cluster provider. Husklet does not yet model
those three mechanisms; they remain explicit gaps and this report does not claim
endpoint-wide prune parity.

The raw-wire contract in `tests/network_prune.rs` covers rejected filter names,
invalid and repeated cutoffs, positive and negative multi-label semantics,
predefined-network preservation, and the exact `NetworksDeleted` response field.
