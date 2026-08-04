# C/Rust differential

## Totals

- both-pass: 8
- c-fail: 84
- c-timeout: 13
- rust-gap: 2180

## Rust gaps by suite, ISA, and exit

| Suite | ISA | Rust status | Rust exit | Count |
|---|---|---|---:|---:|
| completeness | x86_64 | fail | 125 | 155 |
| completeness | aarch64 | fail | 125 | 117 |
| ipc | x86_64 | fail | 125 | 117 |
| ipc | aarch64 | fail | 125 | 116 |
| libc | aarch64 | fail | 125 | 93 |
| libc | x86_64 | fail | 125 | 93 |
| posix | aarch64 | fail | 125 | 91 |
| posix | x86_64 | fail | 125 | 90 |
| filesystem | x86_64 | fail | 125 | 82 |
| filesystem | aarch64 | fail | 125 | 81 |
| syscall | aarch64 | fail | 125 | 81 |
| syscall | x86_64 | fail | 125 | 81 |
| memory | aarch64 | fail | 125 | 77 |
| network | aarch64 | fail | 125 | 76 |
| network | x86_64 | fail | 125 | 76 |
| memory | x86_64 | fail | 125 | 75 |
| signals | aarch64 | fail | 125 | 66 |
| process | aarch64 | fail | 125 | 64 |
| signals | x86_64 | fail | 125 | 63 |
| process | x86_64 | fail | 125 | 62 |
| threads | aarch64 | fail | 125 | 61 |
| threads | x86_64 | fail | 125 | 61 |
| procfs | aarch64 | fail | 125 | 50 |
| procfs | x86_64 | fail | 125 | 50 |
| syscall_edges | aarch64 | fail | 125 | 50 |
| syscall_edges | x86_64 | fail | 125 | 50 |
| time | aarch64 | fail | 125 | 37 |
| time | x86_64 | fail | 125 | 37 |
| isolation | aarch64 | fail | 125 | 14 |
| isolation | x86_64 | fail | 125 | 14 |
