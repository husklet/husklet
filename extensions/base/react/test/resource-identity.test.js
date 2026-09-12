import assert from 'node:assert/strict';
import test from 'node:test';
import React from 'react';
import { RESOURCE_IDENTITY_BYTE_LIMIT, ResourceIdentity } from '../dist/resource-identity.js';
import { Surface, reconciler } from '../dist/reconciler.js';

const h = React.createElement;
function render(node) {
  const frames = [];
  const surface = new Surface((frame) => frames.push(frame));
  const root = reconciler.createContainer(surface, 0, null, false, null, '', () => {}, null);
  reconciler.updateContainer(node, root, null, null);
  return frames.flatMap((frame) => frame.patches);
}

test('ResourceIdentity renders the complete selectable identity instead of an abbreviation', () => {
  const identity = 'c'.repeat(64);
  const patches = render(h(ResourceIdentity, { label: 'Immutable network ID', value: identity }));
  const value = patches.find(
    (patch) => patch.SetProp?.prop === 'Value' && patch.SetProp.value?.Text === identity,
  );
  assert.ok(value, 'the complete identity is published to Code');
  assert.deepEqual(
    patches.findLast(
      (patch) => patch.SetProp?.id === value.SetProp.id && patch.SetProp.prop === 'Wrap',
    )?.SetProp.value,
    { Flag: true },
  );
  assert.equal(
    patches.some(
      (patch) =>
        patch.SetProp?.prop === 'Value' && patch.SetProp.value?.Text === identity.slice(0, 12),
    ),
    false,
  );
});

test('ResourceIdentity refuses empty and unbounded authority text', () => {
  assert.throws(() => render(h(ResourceIdentity, { label: '', value: 'network' })), /label/);
  assert.throws(() => render(h(ResourceIdentity, { label: 'Network', value: '' })), /value/);
  assert.throws(
    () =>
      render(
        h(ResourceIdentity, {
          label: 'Network',
          value: 'x'.repeat(RESOURCE_IDENTITY_BYTE_LIMIT + 1),
        }),
      ),
    /at most 4096 bytes/,
  );
});
