# Permissions scenario ownership audit

This folder translates every case from the former
`tests/scenarios/fixtures/permissions-core.yaml` without changing its stable ID,
image, shell operation, target set, expected exit, class, timeout, expected
failure set, or stdout marker. All 27 legacy cases default to both ARM64 and
AMD64, class `quick`, exit 0, timeout 120 seconds, and no expected failures.

The former ownership chain used for the completed mechanical parity audit was:

- the former `tests/scenarios/fixtures/permissions-core.yaml`: the 27 source contracts;
- the former `tests/scenarios/groups/permissions.rs::group`: loaded that manifest and owned
  the `permissions` group registration;
- the former registry and scenario command modules;
- the 27 `permissions/...` rows in the former generated contract snapshot;
- the former generated image snapshot: shared image provenance for
  `alpine:3.24.1`.

The unified folder runner now owns these contracts. Folder golden files replace
the old inline stdout markers; identical markers are deliberately reused. The
deleted legacy artifacts remain available through repository history.

## Mechanical semantic parity matrix

The folder translation is compared by stable ID after applying each schema's
documented defaults. The audit covers every record and reports mismatches as
individual `(id, field)` pairs.

| Field | Legacy owner | Folder owner | Result |
|---|---|---|---|
| ID | `cases[].id` | `cases[].id` | 27/27 exact |
| OCI image | `cases[].image` | `cases[].image` | 27/27 exact |
| Shell bytes | `run.shell` | `actions[0].shell.script` | 27/27 exact |
| Guest targets | absent means ARM64+AMD64 | absent means ARM64+AMD64 | 27/27 exact |
| Class | absent means `quick` | absent means `quick` | 27/27 exact |
| Timeout | absent means 120 seconds | explicit `timeout: 120` | 27/27 exact |
| Expected failures | absent means none | absent means none | 27/27 exact |
| Exit status | absent means 0 | absent means 0 | 27/27 exact |
| Output markers | inline UTF-8 bytes | local golden bytes, terminal newline excluded for contains matching | 28/28 exact |

The first independent audit found 27 mismatches, all caused by the unified
scenario schema's 180-second default versus the legacy schema's 120-second
default. Making the legacy timeout explicit reduced the mismatch count to zero;
no command or expected-output translation changed.
