import assert from 'node:assert/strict';
import test from 'node:test';
import {
  CONTAINER_DETAIL_SOURCE, CONTAINER_DETAIL_WINDOW_LIMIT, ContainerDetailsSource,
  EXECUTION_DETAIL_SOURCE, EXECUTION_DETAIL_WINDOW_LIMIT, ExecutionDetailsSource,
  IMAGE_DETAIL_SOURCE, IMAGE_DETAIL_WINDOW_LIMIT, ImageDetailsSource,
  NETWORK_DETAIL_SOURCE, NETWORK_DETAIL_WINDOW_LIMIT, NetworkDetailsSource,
  VOLUME_DETAIL_SOURCE, VOLUME_DETAIL_WINDOW_LIMIT, VolumeDetailsSource,
  bounded, boundedMessage, bytes, containerNameError, endpointAliases, immutableContainerId, logText, processRows, resourceReference, shortId,
} from '../src/model.js';

test('container rename validation matches the native byte grammar exactly', () => {
  for (const valid of ['a', 'Worker_2.prod', `a${'-'.repeat(127)}`]) assert.equal(containerNameError(valid), '');
  for (const invalid of ['', '.worker', '-worker', '_worker', 'bad name', 'naïve', `a${'-'.repeat(128)}`]) {
    assert.match(containerNameError(invalid), /1–128 ASCII/);
  }
});

test('endpoint aliases and immutable container identity mirror native boundaries', () => {
  assert.deepEqual(endpointAliases('database.internal, database_2'), ['database.internal', 'database_2']);
  assert.deepEqual(endpointAliases('  '), []);
  for (const invalid of ['same,same', '-leading', 'é', `${'x'.repeat(254)}`, 'one,,two']) {
    assert.throws(() => endpointAliases(invalid), /at most 64 unique/);
  }
  assert.equal(endpointAliases(Array.from({ length: 64 }, (_, index) => `alias-${index}`).join(',')).length, 64);
  assert.equal(immutableContainerId('a'.repeat(32)), true);
  assert.equal(immutableContainerId('a'.repeat(64)), true);
  assert.equal(immutableContainerId('A'.repeat(64)), false);
  assert.equal(immutableContainerId('a'.repeat(63)), false);
  assert.equal(boundedMessage(new Error('x'.repeat(600))).length, 513);
});

test('records are bounded and omissions stay visible', () => {
  const view = bounded(Array.from({ length: 205 }, (_, index) => index));
  assert.equal(view.records.length, 200);
  assert.equal(view.omitted, 5);
});

test('execution metadata is revisioned and served through bounded windows', async () => {
  const mutations = [];
  const source = new ExecutionDetailsSource(async (mutation) => mutations.push(mutation));
  assert.equal(await source.replace({ id: 'e1', container_id: 'c1', running: false, exit_code: 7, pid: 0, command: ['sh', '-c', 'false'], user: 'root' }), 6);
  assert.deepEqual(mutations, [{ Length: { source: EXECUTION_DETAIL_SOURCE, version: 1, rows: 6 } }]);
  const window = source.answer({ source: EXECUTION_DETAIL_SOURCE, version: 1, id: 6, range: { start: 0, count: 999 } });
  assert.equal(window.rows.length, EXECUTION_DETAIL_WINDOW_LIMIT);
  assert.deepEqual(window.rows[0].cells, [{ Text: 'Execution ID' }, { Code: 'e1' }]);
});

test('typed network inspection is revisioned and window bounded', async () => {
  const mutations = [];
  const source = new NetworkDetailsSource(async (mutation) => mutations.push(mutation));
  assert.equal(await source.replace({ id: 'n1', name: 'private', driver: 'bridge', scope: 'local' }), 4);
  assert.deepEqual(mutations, [{ Length: { source: NETWORK_DETAIL_SOURCE, version: 1, rows: 4 } }]);
  const window = source.answer({ source: NETWORK_DETAIL_SOURCE, version: 1, id: 7, range: { start: 0, count: 99 } });
  assert.equal(window.rows.length, NETWORK_DETAIL_WINDOW_LIMIT);
  assert.deepEqual(window.rows[0].cells, [{ Text: 'Network ID' }, { Code: 'n1' }]);
});

test('typed volume inspection exposes only its bounded public fields', async () => {
  const mutations = [];
  const source = new VolumeDetailsSource(async (mutation) => mutations.push(mutation));
  assert.equal(await source.replace({ name: 'cache', driver: 'local', private_field: 'not public' }), 2);
  assert.deepEqual(mutations, [{ Length: { source: VOLUME_DETAIL_SOURCE, version: 1, rows: 2 } }]);
  const window = source.answer({ source: VOLUME_DETAIL_SOURCE, version: 1, id: 8, range: { start: 0, count: 99 } });
  assert.equal(window.rows.length, VOLUME_DETAIL_WINDOW_LIMIT);
  assert.deepEqual(window.rows.map((row) => row.cells[0].Text), ['Name', 'Driver']);
});

test('typed container inspection exposes only authoritative bounded fields', async () => {
  const mutations = [];
  const source = new ContainerDetailsSource(async (mutation) => mutations.push(mutation));
  assert.equal(await source.replace({ id: 'c1', name: 'api', state: 'running', image: 'alpine:3.20', created: 42 }), 5);
  assert.deepEqual(mutations, [{ Length: { source: CONTAINER_DETAIL_SOURCE, version: 1, rows: 5 } }]);
  const window = source.answer({ source: CONTAINER_DETAIL_SOURCE, version: 1, id: 5, range: { start: 0, count: 999 } });
  assert.equal(window.rows.length, CONTAINER_DETAIL_WINDOW_LIMIT);
  assert.deepEqual(window.rows[0].cells, [{ Text: 'Immutable ID' }, { Code: 'c1' }]);
});

test('typed image details become revisioned bounded source windows', async () => {
  const mutations = [];
  const source = new ImageDetailsSource(async (mutation) => mutations.push(mutation));
  const count = await source.replace({
    id: 'sha256:one', references: ['alpine:3.20'], created: 'now', size: 1536,
    os: 'linux', architecture: 'amd64', entrypoint: ['/bin/sh'], command: ['-c', 'true'],
    working_directory: '/work', user: '',
  });
  assert.equal(count, 10);
  assert.deepEqual(mutations, [{ Length: { source: IMAGE_DETAIL_SOURCE, version: 1, rows: 10 } }]);
  const window = source.answer({ source: IMAGE_DETAIL_SOURCE, version: 1, id: 3, range: { start: 0, count: 10_000 } });
  assert.equal(window.rows.length, IMAGE_DETAIL_WINDOW_LIMIT);
  assert.ok(window.rows.length <= IMAGE_DETAIL_WINDOW_LIMIT);
  assert.deepEqual(window.rows[0].cells, [{ Text: 'ID' }, { Code: 'sha256:one' }]);
  assert.equal(source.answer({ source: IMAGE_DETAIL_SOURCE, version: 0, id: 4, range: { start: 0, count: 1 } }), null);
});

test('wire-shaped process matrices retain their host titles', () => {
  assert.deepEqual(processRows({ titles: ['PID', 'USER', 'CMD'], processes: [['7', 'root', 'sleep 5']] }, 'api'), [
    { container: 'api', cells: { PID: '7', USER: 'root', CMD: 'sleep 5' }, values: ['7', 'root', 'sleep 5'] },
  ]);
});

test('display helpers tolerate real host representation variants', () => {
  assert.equal(shortId('123456789012345'), '123456789012');
  assert.equal(bytes(1536), '1.5 KiB');
  assert.equal(logText({ stdout: [111, 107], stderr: new Uint8Array([33]) }), 'ok\n!');
  assert.equal(resourceReference({ id: 'opaque', name: 'friendly' }), 'opaque');
  assert.equal(resourceReference({ name: 'friendly' }), 'friendly');
});
