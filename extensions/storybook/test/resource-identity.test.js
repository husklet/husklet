import assert from 'node:assert/strict';
import test from 'node:test';
import { createElement as h } from 'react';
import { ResourceIdentityStory } from '../dist/resource-identity.js';
import { host } from './host.js';

test('ResourceIdentity page demonstrates one complete network authority', () => {
  const frame = host().render(h(ResourceIdentityStory));
  const values = frame.patches
    .filter((patch) => patch.SetProp?.prop === 'Value')
    .map((patch) => patch.SetProp.value?.Text);
  assert.deepEqual(values, ['c'.repeat(64)]);
  assert.equal(values.includes('c'.repeat(12)), false);
});
