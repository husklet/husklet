# Daemon compatibility scenarios

This integration test is the local end-to-end compatibility runner. It exercises the public headless API and
the separately built daemon/client executables. It does not inspect source text and does not count a
scenario as passing until the behavior has run successfully.

Run the quick suite with:

```text
nix develop --command cargo test -p hl-daemon --test scenarios -- quick
```
