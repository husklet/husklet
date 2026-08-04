# Terminal scenario oracle

These 43 end-to-end cases preserve the IDs, images, shell or argv commands,
timeouts, class, targets, expected failures, resources, environments, exit
status, and substring contracts from
`tests/scenarios/fixtures/terminal-core.yaml`. The 36 cases that used the
legacy terminal executor declare `pty`; the remaining seven retain ordinary
pipe-backed execution. The existing Bash argv case remains an argv action.

Each expected substring is stored under `golden/` with no trailing line feed.
That detail is load-bearing: terminal output commonly contains CRLF, while the
legacy inline marker matched only its literal bytes. Adding LF to a golden file
would turn a representation migration into a different output contract.

The legacy terminal runner joined stdout and stderr before matching. Commands
whose expected text can originate on stderr already perform their own explicit
redirection; no migrated marker requires an added channel bridge. The repository
runner checks expected markers on stdout. Its typed PTY adapter supplies a PTY,
default `TERM=xterm`, and the 24x80 initial window required by the 36 `pty`
cases.

The legacy image fixture applied OCI-configured environment and working-directory
metadata before case overrides. In particular, it injected `TERM=xterm` only
when neither the image environment nor the case environment defined `TERM`.
`TestImage` currently materializes only the root filesystem, so the repository
runner checks only case environment before supplying that default and cannot yet
inherit either OCI environment or working directory. These cases use explicit
commands, `/` as their effective directory, and only the declared `TERM=screen`
override, but restoring generic OCI metadata inheritance remains a runner
capability gap and is not papered over in YAML.

No case contains a C, Rust, or heredoc source payload, so this category needs no
`source/` directory. This is a representation and ownership migration only. It
changes no engine runtime behavior, so the retired C implementation was not used
as an implementation oracle and `/Users/x/dd/engine` was not modified.
