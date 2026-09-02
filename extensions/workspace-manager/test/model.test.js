import assert from 'node:assert/strict';
import test from 'node:test';
import {
  CONTAINER_DETAIL_SOURCE, CONTAINER_DETAIL_WINDOW_LIMIT, ContainerDetailsSource,
  IMAGE_DETAIL_SOURCE, IMAGE_DETAIL_WINDOW_LIMIT, ImageDetailsSource,
  bounded, bytes, logText, processRows, resourceReference, shortId,
} from '../src/model.js';

test('records are bounded and omissions stay visible', () => {
  const view = bounded(Array.from({ length: 205 }, (_, index) => index));
  assert.equal(view.records.length, 200);
  assert.equal(view.omitted, 5);
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
