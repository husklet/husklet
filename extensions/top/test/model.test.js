import assert from 'node:assert/strict';
import test from 'node:test';
import {
  CONTAINER_DETAIL_SOURCE,
  CONTAINER_DETAIL_WINDOW_LIMIT,
  ContainerDetailsSource,
  EXECUTION_DETAIL_SOURCE,
  EXECUTION_DETAIL_WINDOW_LIMIT,
  ExecutionDetailsSource,
  IMAGE_DETAIL_SOURCE,
  IMAGE_DETAIL_WINDOW_LIMIT,
  ImageDetailsSource,
  NETWORK_DETAIL_SOURCE,
  NETWORK_DETAIL_WINDOW_LIMIT,
  NetworkDetailsSource,
  PROCESS_TABLE_SOURCE,
  PROCESS_TABLE_WINDOW_LIMIT,
  ProcessTableSource,
  VOLUME_DETAIL_SOURCE,
  VOLUME_DETAIL_WINDOW_LIMIT,
  VolumeDetailsSource,
  bounded,
  boundedMessage,
  bytes,
  containerNameError,
  endpointAliases,
  immutableContainerId,
  logText,
  processRows,
  processTableSchema,
  resourceReference,
  shortId,
} from '../dist/model.js';

test('process table exposes supplied metrics and serves only requested bounded windows', async () => {
  const mutations = [];
  const source = new ProcessTableSource(async (mutation) => mutations.push(mutation));
  const records = Array.from({ length: 10_000 }, (_, index) => ({
    container: index % 2 ? 'worker' : 'api',
    cells: {
      PID: String(index + 1),
      USER: index % 2 ? 'builder' : 'root',
      '%CPU': `${index % 100}.5`,
      RSS: String(4096 + index),
      COMMAND: `job-${index}`,
    },
    values: [String(index + 1), index % 2 ? 'builder' : 'root', `job-${index}`],
  }));
  const schema = processTableSchema(records);
  assert.deepEqual(
    schema.map(({ key }) => key),
    ['container', 'pid', 'user', 'cpu', 'memory', 'command'],
  );
  assert.equal(schema.find(({ key }) => key === 'container').identity, true);
  for (const key of ['user', 'cpu', 'memory']) {
    assert.equal(schema.find((column) => column.key === key).importance, 'optional');
  }
  assert.equal(await source.replace(records, schema, 'worker', 'pid', true), 5_000);
  assert.deepEqual(mutations, [
    { Length: { source: PROCESS_TABLE_SOURCE, version: 1, rows: 5_000 } },
  ]);
  const window = source.answer({
    source: PROCESS_TABLE_SOURCE,
    version: 1,
    id: 44,
    range: { start: 0, count: 10_000 },
  });
  assert.equal(window.rows.length, PROCESS_TABLE_WINDOW_LIMIT);
  assert.equal(source.generated, PROCESS_TABLE_WINDOW_LIMIT);
  assert.deepEqual(window.rows[0].cells, [
    { Text: 'worker' },
    { Text: '10000' },
    { Text: 'builder' },
    { Text: '99.5' },
    { Text: '14095' },
    { Text: 'job-9999' },
  ]);
  assert.equal(
    source.answer({
      source: PROCESS_TABLE_SOURCE,
      version: 0,
      id: 45,
      range: { start: 0, count: 1 },
    }),
    null,
  );
});

test('process table omits metric columns the daemon did not supply', async () => {
  const records = processRows({ titles: ['PID', 'COMMAND'], processes: [['7', 'sleep 5']] }, 'api');
  const schema = processTableSchema(records);
  assert.deepEqual(
    schema.map(({ key }) => key),
    ['container', 'pid', 'command'],
  );
  const source = new ProcessTableSource();
  await source.replace(records, schema);
  assert.equal(
    source.accepts({
      source: PROCESS_TABLE_SOURCE,
      version: source.version,
      column: 'pid',
      descending: false,
    }),
    true,
  );
  assert.equal(
    source.accepts({
      source: PROCESS_TABLE_SOURCE,
      version: source.version - 1,
      column: 'pid',
      descending: false,
    }),
    false,
  );
});

test('container rename validation matches the native byte grammar exactly', () => {
  for (const valid of ['a', 'Worker_2.prod', `a${'-'.repeat(127)}`])
    assert.equal(containerNameError(valid), '');
  for (const invalid of [
    '',
    '.worker',
    '-worker',
    '_worker',
    'bad name',
    'naïve',
    `a${'-'.repeat(128)}`,
  ]) {
    assert.match(containerNameError(invalid), /1–128 ASCII/);
  }
});

test('endpoint aliases and immutable container identity mirror native boundaries', () => {
  assert.deepEqual(endpointAliases('database.internal, database_2'), [
    'database.internal',
    'database_2',
  ]);
  assert.deepEqual(endpointAliases('  '), []);
  for (const invalid of ['same,same', '-leading', 'é', `${'x'.repeat(254)}`, 'one,,two']) {
    assert.throws(() => endpointAliases(invalid), /at most 64 unique/);
  }
  assert.equal(
    endpointAliases(Array.from({ length: 64 }, (_, index) => `alias-${index}`).join(',')).length,
    64,
  );
  assert.equal(immutableContainerId('a'.repeat(32)), true);
  assert.equal(immutableContainerId('a'.repeat(64)), true);
  assert.equal(immutableContainerId('A'.repeat(64)), false);
  assert.equal(immutableContainerId('a'.repeat(63)), false);
  assert.equal(boundedMessage(new Error('x'.repeat(600))).length, 513);
});

test('transport sequencing failures become actionable product language', () => {
  assert.equal(
    boundedMessage(
      new Error('expected frame 9, received 12: extension catalogue transport closed'),
    ),
    'Connection to Husklet was interrupted. Retry the operation.',
  );
  assert.equal(
    boundedMessage(new Error('extension catalogue transport closed')),
    'The extension catalogue connection closed before it completed. Retry the catalogue.',
  );
  assert.equal(
    boundedMessage(new Error('registry refused credentials')),
    'registry refused credentials',
  );
});

test('records are bounded and omissions stay visible', () => {
  const view = bounded(Array.from({ length: 205 }, (_, index) => index));
  assert.equal(view.records.length, 200);
  assert.equal(view.omitted, 5);
});

test('execution metadata is revisioned and served through bounded windows', async () => {
  const mutations = [];
  const source = new ExecutionDetailsSource(async (mutation) => mutations.push(mutation));
  assert.equal(
    await source.replace({
      id: 'e1',
      container_id: 'c1',
      running: false,
      exit_code: 7,
      result: { kind: 'code', value: 7 },
      created_at_ms: 1_000,
      started_at_ms: null,
      finished_at_ms: 1_100,
      pid: 0,
      command: ['sh', '-c', 'false'],
      user: 'root',
    }),
    8,
  );
  assert.deepEqual(mutations, [
    { Length: { source: EXECUTION_DETAIL_SOURCE, version: 1, rows: 8 } },
  ]);
  const window = source.answer({
    source: EXECUTION_DETAIL_SOURCE,
    version: 1,
    id: 6,
    range: { start: 0, count: 999 },
  });
  assert.equal(window.rows.length, EXECUTION_DETAIL_WINDOW_LIMIT);
  assert.deepEqual(window.rows[0].cells, [{ Text: 'Execution ID' }, { Code: 'e1' }]);
});

test('typed network inspection is revisioned and window bounded', async () => {
  const mutations = [];
  const source = new NetworkDetailsSource(async (mutation) => mutations.push(mutation));
  assert.equal(
    await source.replace({
      id: 'n1',
      name: 'private',
      driver: 'bridge',
      scope: 'local',
      kind: 'custom',
    }),
    4,
  );
  assert.deepEqual(mutations, [{ Length: { source: NETWORK_DETAIL_SOURCE, version: 1, rows: 4 } }]);
  const window = source.answer({
    source: NETWORK_DETAIL_SOURCE,
    version: 1,
    id: 7,
    range: { start: 0, count: 99 },
  });
  assert.equal(window.rows.length, NETWORK_DETAIL_WINDOW_LIMIT);
  assert.deepEqual(window.rows[0].cells, [{ Text: 'Network ID' }, { Code: 'n1' }]);
});

test('typed volume inspection exposes only its bounded public fields', async () => {
  const mutations = [];
  const source = new VolumeDetailsSource(async (mutation) => mutations.push(mutation));
  assert.equal(
    await source.replace({ name: 'cache', driver: 'local', private_field: 'not public' }),
    2,
  );
  assert.deepEqual(mutations, [{ Length: { source: VOLUME_DETAIL_SOURCE, version: 1, rows: 2 } }]);
  const window = source.answer({
    source: VOLUME_DETAIL_SOURCE,
    version: 1,
    id: 8,
    range: { start: 0, count: 99 },
  });
  assert.equal(window.rows.length, VOLUME_DETAIL_WINDOW_LIMIT);
  assert.deepEqual(
    window.rows.map((row) => row.cells[0].Text),
    ['Name', 'Driver'],
  );
});

test('typed container inspection exposes only authoritative bounded fields', async () => {
  const mutations = [];
  const source = new ContainerDetailsSource(async (mutation) => mutations.push(mutation));
  assert.equal(
    await source.replace({
      id: 'c1',
      name: 'api',
      state: 'running',
      image: 'alpine:3.20',
      created: 42,
    }),
    5,
  );
  assert.deepEqual(mutations, [
    { Length: { source: CONTAINER_DETAIL_SOURCE, version: 1, rows: 5 } },
  ]);
  const window = source.answer({
    source: CONTAINER_DETAIL_SOURCE,
    version: 1,
    id: 5,
    range: { start: 0, count: 999 },
  });
  assert.equal(window.rows.length, CONTAINER_DETAIL_WINDOW_LIMIT);
  assert.deepEqual(window.rows[0].cells, [{ Text: 'Immutable ID' }, { Code: 'c1' }]);
});

test('typed image details become revisioned bounded source windows', async () => {
  const mutations = [];
  const source = new ImageDetailsSource(async (mutation) => mutations.push(mutation));
  const count = await source.replace({
    id: 'sha256:one',
    references: ['alpine:3.20'],
    created: 'now',
    size: 1536,
    os: 'linux',
    architecture: 'amd64',
    entrypoint: ['/bin/sh'],
    command: ['-c', 'true'],
    working_directory: '/work',
    user: '',
  });
  assert.equal(count, 10);
  assert.deepEqual(mutations, [{ Length: { source: IMAGE_DETAIL_SOURCE, version: 1, rows: 10 } }]);
  const window = source.answer({
    source: IMAGE_DETAIL_SOURCE,
    version: 1,
    id: 3,
    range: { start: 0, count: 10_000 },
  });
  assert.equal(window.rows.length, IMAGE_DETAIL_WINDOW_LIMIT);
  assert.ok(window.rows.length <= IMAGE_DETAIL_WINDOW_LIMIT);
  assert.deepEqual(window.rows[0].cells, [{ Text: 'ID' }, { Code: 'sha256:one' }]);
  assert.equal(
    source.answer({
      source: IMAGE_DETAIL_SOURCE,
      version: 0,
      id: 4,
      range: { start: 0, count: 1 },
    }),
    null,
  );
});

test('detail sources reject malformed host row requests before slicing', async () => {
  const source = new ImageDetailsSource();
  await source.replace({
    id: 'sha256:one',
    references: [],
    created: 'now',
    size: 1,
    os: 'linux',
    architecture: 'amd64',
    entrypoint: [],
    command: [],
    working_directory: '/',
    user: '',
  });
  for (const request of [
    null,
    {},
    { source: IMAGE_DETAIL_SOURCE, version: 1, id: 1 },
    { source: IMAGE_DETAIL_SOURCE, version: 1, id: 1, range: { start: -1, count: 1 } },
    { source: IMAGE_DETAIL_SOURCE, version: 1, id: 1, range: { start: 0, count: Number.NaN } },
  ]) {
    assert.equal(source.answer(request), null);
  }
});

test('wire-shaped process matrices retain their host titles', () => {
  assert.deepEqual(
    processRows({ titles: ['PID', 'USER', 'CMD'], processes: [['7', 'root', 'sleep 5']] }, 'api'),
    [
      {
        container: 'api',
        cells: { PID: '7', USER: 'root', CMD: 'sleep 5' },
        values: ['7', 'root', 'sleep 5'],
      },
    ],
  );
});

test('display helpers tolerate real host representation variants', () => {
  assert.equal(shortId('123456789012345'), '123456789012');
  assert.equal(bytes(1536), '1.5 KiB');
  assert.equal(logText({ stdout: [111, 107], stderr: new Uint8Array([33]) }), 'ok\n!');
  assert.equal(resourceReference({ id: 'opaque', name: 'friendly' }), 'opaque');
  assert.equal(resourceReference({ name: 'friendly' }), 'friendly');
});
