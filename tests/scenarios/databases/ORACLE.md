# Database scenario oracle

This directory owns the 39 database contracts from
`tests/scenarios/fixtures/databases-core.yaml` that the repository scenario
executor can run directly. It preserves each stable ID, exact OCI image,
quick/long class, both default target ISAs, timeout, expected exit, command,
and output substring. The 34 server cases retain `host_port`; the five
version or in-memory SQLite cases retain no explicit resource. Expected
substring bytes are stored without a trailing line feed under `golden/`.

Exactly five of the 44 legacy cases are not copied into `test.yaml` because
they require the typed readiness lifecycle already represented by the schema
but still rejected by `validate_supported` in the executor:

- `databases/mongo-agg-7`: startup `mongod --bind_ip 127.0.0.1 --fork --logpath /tmp/mongo.log`, probe `mongosh 'mongodb://%2Ftmp%2Fmongodb-27017.sock' --quiet --eval 'db.runCommand({ping:1}).ok'`, attempts 3, delay 1000 ms, logs `/tmp/mongo.log`.
- `databases/mongo-count-7`: startup `mongod --bind_ip 127.0.0.1 --fork --logpath /tmp/mongo.log`, probe `mongosh 'mongodb://%2Ftmp%2Fmongodb-27017.sock' --quiet --eval 'db.runCommand({ping:1}).ok'`, attempts 3, delay 1000 ms, logs `/tmp/mongo.log`.
- `databases/mongo-parallel-readiness-7`: startup `mongod --bind_ip 127.0.0.1 --fork --logpath /tmp/mongo.log`, probe `test -S /tmp/mongodb-27017.sock`, attempts 1, delay 0 ms, logs `/tmp/mongo.log`.
- `databases/mongo-filter-count-7`: startup `mongod --bind_ip 127.0.0.1 --fork --logpath /tmp/mongo.log`, probe `mongosh 'mongodb://%2Ftmp%2Fmongodb-27017.sock' --quiet --eval 'db.runCommand({ping:1}).ok'`, attempts 3, delay 1000 ms, logs `/tmp/mongo.log`.
- `databases/mongo-version-8`: startup `mongod --bind_ip 127.0.0.1 --fork --logpath /tmp/mongo.log`, probe `mongosh 'mongodb://%2Ftmp%2Fmongodb-27017.sock' --quiet --eval 'db.runCommand({ping:1}).ok'`, attempts 3, delay 1000 ms, logs `/tmp/mongo.log`.

Omitting these cases is an explicit executor-capability gap, not a passing or
unsupported verdict. Their startup, probe, retry/delay, diagnostic-log, image,
command, and output contracts remain authoritative in the legacy fixture until
the ordered readiness adapter lands.

The two SQLite cases formerly embedded Python programs in shell heredocs.
Those exact payload bytes now live in `source/sqlite_join.py` and
`source/sqlite_query.py`, are installed at matching `/tmp` paths, and execute
with `python3`. The two NATS argv cases previously relied on OCI entrypoint
prefixing; they now name `/nats-server` explicitly because `TestImage` does not
carry entrypoint metadata. The etcd argv was already complete and is unchanged.

The old category scheduler used `host_port` as a fallback when a selected case
had no declared resource, while its inner runner still ran cases concurrently.
The new per-case definitions retain the 34 explicit `host_port` declarations;
the five non-server probes remain light. The repository runner still lacks OCI
environment, working-directory, and user inheritance, which is recorded as a
generic runner gap rather than hidden in these definitions.

`cleanup_probe` in the old Rust database group is a container lifecycle public-
contract test, not an image scenario. The owning crate already exercises the
same force-remove and post-removal lookup contract in
`rename_wait_removed_stop_and_force_remove_follow_owned_lifecycle` under
`hl-container`'s lifecycle tests, so it is intentionally not duplicated as
YAML here.

This is a representation and ownership migration only. It changes no engine
runtime behavior, so the retired C implementation was not used as an
implementation oracle and `/Users/x/dd/engine` was not modified.
