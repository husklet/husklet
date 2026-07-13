# Rebrand decision register

Resolve these before any rename patch. A decision must name the compatibility policy and the tests that
prove it; “replace all” is not sufficient for cross-process or persisted contracts.

| Decision | Proposed default | Why it is blocking |
|---|---|---|
| user CLI binary | `husklet` | packaging, docs, launchers and shell UX depend on it |
| internal binaries | `husklet-daemon`, `husklet-display`, `husklet-compositor`, `husklet-app`, `husklet-term` | Cargo targets, process discovery and bundle resources change together |
| service namespace | `com.husklet.*` | launchd, bundle IDs and Mach bootstrap consumers must agree |
| daemon volume env | `HL_VOLUMES_DIR` | avoids collision with engine volume-list semantics |
| engine sandbox env | use semantic names rather than merging both old variables | prevents two controls collapsing accidentally |
| wire magic values | keep numeric values initially; rename identifiers only | avoids cosmetic protocol break during brand cutover |
| archive filenames | keep `dd-manifest.json`/`dd-image.json` for one format version, or version/migrate explicitly | existing archives otherwise stop loading |
| state root | fresh `~/.husklet`; old root rejected with actionable guidance | avoids silently running two daemons/stores |
| xattrs | decide migration or fresh-root rejection | copied images can retain old keys |
| external image name | replace only with a published husklet image/digest | cannot rename a remote reference that does not exist |

The user has already selected the product name **husklet**, `HL_*` environment prefix, flat `hl_*` internal
symbol prefix, `husklet-*` package names, and a fresh state-root cutover. The table narrows remaining
execution choices; it does not reopen the brand decision.
