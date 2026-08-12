# Linter

`hl-lint` runs clang-format, clang-tidy, cppcheck, and deterministic C policy
checks. The driver launches analyzers directly, without a shell, and bounds
captured output. Native POSIX and Windows backends preserve argument boundaries
and merge analyzer output in emission order.

Run the repository check:

```sh
nix build .#checks.$(nix eval --impure --raw --expr builtins.currentSystem).lint
```

Engine code must read environment variables through the central configuration
boundary and emit diagnostics through the tagged service in `include/hl/log.h`.
Direct `getenv`, console `printf`, `puts`, `perror`, and writes to
`stdout`/`stderr` are policy errors. Buffer formatting and serialization to an
ordinary file remain valid.

Exit 0 means every enabled stage completed without policy or infrastructure
errors. Analyzer findings are warnings unless strict mode is enabled; invalid
arguments, missing tools, spawn failures, and policy violations are always
fatal.

Rules live in `src/`, with contract tests in `tests/`. Add or change a rule and
its positive and negative test cases together. `ctest -L lint
--no-tests=error` runs those contracts from a configured test build.
