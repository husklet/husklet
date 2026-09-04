import assert from 'node:assert/strict';
import test from 'node:test';

import * as client from '@husklet/client';
import * as react from '../src/index.js';

test('React preserves the framework-neutral client exports by identity', () => {
  for (const [name, value] of Object.entries(client)) {
    if (name === 'connect') continue;
    assert.equal(react[name], value, `@husklet/react is missing the client ${name} export`);
  }
  assert.notEqual(react.connect, client.connect, 'React retains its render-aware connect override');
});
