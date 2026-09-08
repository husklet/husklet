import assert from 'node:assert/strict';
import test from 'node:test';

import { createWindowCache } from '../dist/index.js';

test('window cache bounds million-row viewport churn and refreshes recent windows', () => {
  const cache = createWindowCache(3);
  cache.set('0', ['selected']);
  cache.set('128', ['next']);
  assert.deepEqual(cache.get('0'), ['selected']);
  cache.set('999744', ['near-end']);
  cache.set('999872', ['end']);
  assert.equal(cache.size, 3);
  assert.equal(cache.get('128'), undefined, 'least-recently-used window is evicted');
  assert.deepEqual(cache.get('0'), ['selected'], 'recent selection window remains stable');
  assert.throws(() => createWindowCache(0), /between 1 and 1024/);
  assert.throws(() => createWindowCache(1025), /between 1 and 1024/);
});
