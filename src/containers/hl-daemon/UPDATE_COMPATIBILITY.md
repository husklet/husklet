# Docker container-update compatibility

Docker Engine API 1.43 treats zero-valued numeric resource members in an update
request as unspecified unless the field's update contract explicitly defines a
zero transition. Docker CLI 29.1.3 demonstrates this wire contract by sending
`Memory=0` and `NanoCpus=0` during `docker update --restart=...`, even when the
user requested no resource change.

For update only, `Memory=0` and `NanoCpus=0` therefore preserve the stored
limits. Positive values remain effective while the container is stopped, and
the container owner refuses live resource changes because the Rust engine
projects `HL_MEM_MAX` and `HL_CPUS` at launch. This differs from create, where
zero selects the initial unlimited/platform-default resource value. Nullable
`PidsLimit` remains absent when the client did not request it; its explicit
`0`/`-1` unlimited contract is unchanged.

`hl-container` owns durable resource mutation and restart-policy transitions.
The retained C engine has no Docker/container update domain; its resource and
launch configuration is construction-scoped. No retained C runtime behavior is
changed by this request-model correction.
