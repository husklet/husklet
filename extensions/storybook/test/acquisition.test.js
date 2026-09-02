import assert from 'node:assert/strict';
import test from 'node:test';

import { acquisitionStates } from '../src/acquisition.js';

test('acquisition examples preserve the user-visible semantic lifecycle', () => {
  assert.deepEqual(
    acquisitionStates.map((state) => state.key),
    ['checking', 'pulling-indeterminate', 'pulling-determinate', 'manifest', 'failure', 'ready'],
  );
  const byKey = Object.fromEntries(acquisitionStates.map((state) => [state.key, state]));
  assert.match(byKey['pulling-indeterminate'].status, /progress unavailable/);
  assert.match(byKey['pulling-determinate'].status, /25%; 25 of 100 bytes/);
  assert.deepEqual(byKey.failure.actions, ['Retry']);
  assert.deepEqual(byKey.ready.actions, ['Install', 'Cancel']);
  assert.ok(
    ['checking', 'pulling-indeterminate', 'pulling-determinate', 'manifest'].every((key) =>
      byKey[key].actions.includes('Cancel download'),
    ),
  );
});
