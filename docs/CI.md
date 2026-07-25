# Continuous integration

Hosted CI runs the deterministic workspace checks and the bounded real-image
smoke matrix. The exhaustive compatibility census is intentionally manual until
its persistent runner is provisioned and independently verified. A release must
not depend on a self-hosted runner that has not been proven online.

## Exhaustive census runner

The manual `Full scenario census` workflow requires a dedicated Apple Silicon
macOS runner with all of these labels:

- `self-hosted`
- `macOS`
- `ARM64`
- `husklet-census`

Provisioning checklist:

1. Register the runner for this repository and dedicate it to trusted Husklet
   workflows. Do not allow pull-request workflows from forks to target it.
2. Install Nix and verify that `nix develop "path:$PWD/nix"` works in the
   runner service account.
3. Create `/Users/Shared/husklet/oci-census/{arm64,amd64}` on persistent
   storage. Each architecture needs its own image catalog because one canonical
   OCI reference resolves to one platform manifest. The complete stores are
   substantially larger than the GitHub Actions repository cache quota and
   must not use `actions/cache`.
4. Seed both platforms using authenticated registry credentials:

   ```sh
   export HL_REGISTRY_USERNAME=...
   export HL_REGISTRY_PASSWORD=...
   export HL_SCENARIO_IMAGE_CACHE=/Users/Shared/husklet/oci-census/arm64
   cargo test -p hl-daemon --test scenarios -- all --prefetch --jobs 8 --target arm64
   export HL_SCENARIO_IMAGE_CACHE=/Users/Shared/husklet/oci-census/amd64
   cargo test -p hl-daemon --test scenarios -- all --prefetch --jobs 8 --target amd64
   ```

5. Remove registry credentials from the runner environment. Prove the store is
   complete and platform-correct:

   ```sh
   export HL_SCENARIO_OFFLINE=1
   export HL_SCENARIO_IMAGE_CACHE=/Users/Shared/husklet/oci-census/arm64
   cargo test -p hl-daemon --test scenarios -- cache-preflight arm64
   export HL_SCENARIO_IMAGE_CACHE=/Users/Shared/husklet/oci-census/amd64
   cargo test -p hl-daemon --test scenarios -- cache-preflight amd64
   ```

6. Verify through the authenticated GitHub Actions runners API that a runner
   carrying every required label reports `online`.
7. Dispatch `Full scenario census` and require successful arm64 and amd64
   offline results.

Only after all seven checks have current evidence may the census workflow be
added as a required release dependency. Until then, release stability relies on
the required hosted deterministic checks and bounded authenticated/offline
smoke matrix.
