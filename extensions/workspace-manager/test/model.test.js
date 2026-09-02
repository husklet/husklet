import assert from 'node:assert/strict';
import test from 'node:test';
import { bounded, bytes, logText, processRows, resourceReference, shortId } from '../src/model.js';

test('records are bounded and omissions stay visible', () => {
  const view = bounded(Array.from({ length: 205 }, (_, index) => index));
  assert.equal(view.records.length, 200);
  assert.equal(view.omitted, 5);
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
