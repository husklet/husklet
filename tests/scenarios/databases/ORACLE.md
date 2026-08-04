# Database scenario oracle

This directory owns all 44 database contracts formerly declared in
`tests/scenarios/fixtures/databases-core.yaml` that the repository scenario
executor can run directly. It preserves each stable ID, exact OCI image,
quick/long class, both default target ISAs, timeout, expected exit, command,
and output substring. The 39 server cases retain `host_port`; the five version
or in-memory SQLite cases retain no explicit resource. Existing expected
substring files remain byte-identical; the final Mongo values include their
ordinary line terminator because each command emits a complete output line.

Three additional folder-owned cases preserve the retired `realsw` workflow's
Redis round trip and increment, PostgreSQL initialization and aggregation, and
NATS daemon-readiness contracts. Typed readiness replaces fixed sleeps while
retaining its images, guest commands, markers, timeout, isolation, and bounded
per-case cleanup.

The final five Mongo cases use the typed readiness lifecycle:

- `databases/mongo-agg-7`: startup `mongod --bind_ip 127.0.0.1 --fork --logpath /tmp/mongo.log`, probe `mongosh 'mongodb://%2Ftmp%2Fmongodb-27017.sock' --quiet --eval 'db.runCommand({ping:1}).ok'`, attempts 3, delay 1000 ms, logs `/tmp/mongo.log`.
- `databases/mongo-count-7`: startup `mongod --bind_ip 127.0.0.1 --fork --logpath /tmp/mongo.log`, probe `mongosh 'mongodb://%2Ftmp%2Fmongodb-27017.sock' --quiet --eval 'db.runCommand({ping:1}).ok'`, attempts 3, delay 1000 ms, logs `/tmp/mongo.log`.
- `databases/mongo-parallel-readiness-7`: startup `mongod --bind_ip 127.0.0.1 --fork --logpath /tmp/mongo.log`, probe `test -S /tmp/mongodb-27017.sock`, attempts 1, delay 0 ms, logs `/tmp/mongo.log`.
- `databases/mongo-filter-count-7`: startup `mongod --bind_ip 127.0.0.1 --fork --logpath /tmp/mongo.log`, probe `mongosh 'mongodb://%2Ftmp%2Fmongodb-27017.sock' --quiet --eval 'db.runCommand({ping:1}).ok'`, attempts 3, delay 1000 ms, logs `/tmp/mongo.log`.
- `databases/mongo-version-8`: startup `mongod --bind_ip 127.0.0.1 --fork --logpath /tmp/mongo.log`, probe `mongosh 'mongodb://%2Ftmp%2Fmongodb-27017.sock' --quiet --eval 'db.runCommand({ping:1}).ok'`, attempts 3, delay 1000 ms, logs `/tmp/mongo.log`.

The repository executor now runs startup once, probes in declared order with
the exact retry delay, and includes the requested guest log if readiness never
succeeds. Their stable IDs, images, commands, output values, and readiness
metadata are therefore folder-owned rather than retained as a legacy gap.

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
