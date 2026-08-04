# Utility scenario provenance

This directory owns all 302 executable contracts formerly declared in
tests/scenarios/fixtures/utilities-core.yaml. The stable IDs, exact image
references, quick class, both target ISAs, 120-second timeout, manifest
environment, expected-failure state, exit status, command arguments, and
output bytes are preserved. Expected output is owned under golden/.

`utilities/hello-world` remains an entrypoint action against
`hello-world:latest`. The repository image materializer now retains OCI
ENTRYPOINT/CMD metadata and the execution adapter uses that typed runtime
configuration for the container's initial process, so no guessed shell command
or executable path replaces the original contract.

The seven utilities/compile-* heredoc payloads are byte-identical files in
source/ and are installed at named /tmp/p.* paths. Their compiler flags and
result programs are unchanged. Image-entrypoint argument cases name their
known executable explicitly because the new runner executes a complete argv
rather than prepending image metadata. The 14 affected mappings are:

- `utilities/openssl-sha256-empty`, `utilities/openssl-version`: prepend
  `openssl`;
- `utilities/bash-base-convert`, `utilities/bash-brace-arith`,
  `utilities/bash-arrays`, `utilities/bash-param-expand`, and
  `utilities/bash-version`: execute their already complete `bash` argv without
  the image entrypoint wrapper;
- `utilities/jq-add`, `utilities/jq-object`, `utilities/jq-sort`, and
  `utilities/jq-version`: prepend `jq`;
- `utilities/git-version`: prepend `git`;
- `utilities/curl-version`: prepend `curl`;
- `utilities/socat-version`: prepend `socat`.

This explicit executable bridge preserves the requested program arguments,
but it is not byte-identical argv metadata. The materialized image now carries
OCI environment, working-directory, user, ENTRYPOINT, and CMD metadata into
the process adapter. The runner also searches the same concatenated stdout and
stderr byte stream as the legacy `stdout_contains` checker.

The legacy utilities scheduler had no category fallback and no declared
resources. The migrated cases add process_heavy to the seven compiler cases
and host_port to utilities/nc-loopback and utilities/wget-loopback, making the
real parallel-execution constraints explicit. These are operational
scheduling annotations, not guest-visible behavior changes.

The directory-local YAML is now the only declarative owner; the pure legacy
loader/runner wrapper was removed. This is a scenario-definition migration. It
does not claim that the Rust engine passes the cases, and it does not modify or
rely upon retained C engine implementation.
