import assert from 'node:assert/strict';
import test from 'node:test';
import React from 'react';

import { Playground } from '../dist/app.js';
import { CONFIRMATION_STORY, ConfirmationStory } from '../dist/confirmation.js';
import { host } from './host.js';

const h = React.createElement;
const settle = () => new Promise((resolve) => setImmediate(resolve));

function labelled(patches, label) {
  let found;
  for (const patch of patches) {
    if (patch.SetProp?.prop === 'Label' && patch.SetProp.value?.Text === label)
      found = patch.SetProp.id;
  }
  return found;
}

function invoke(stage, label) {
  const patches = stage.frames.flatMap((frame) => frame.patches);
  const id = labelled(patches, label);
  assert.ok(id, `missing ${label}`);
  assert.equal(stage.surface.dispatch({ trigger: 'Invoke', node: id, id: `${id}:Invoke` }), true);
}

test('confirmation story demonstrates reveal, separate authority, and success', async () => {
  const stage = host();
  stage.render(h(ConfirmationStory));
  invoke(stage, 'Remove volume');
  const open = stage.frames.flatMap((frame) => frame.patches);
  assert.ok(labelled(open, 'Remove volume cache generation 7? This cannot be undone.'));
  const confirmation = labelled(open, 'Confirm removal');
  assert.ok(
    open.some(
      (patch) =>
        patch.SetProp?.id === confirmation &&
        patch.SetProp.prop === 'Destructive' &&
        patch.SetProp.value?.Flag,
    ),
  );
  invoke(stage, 'Confirm removal');
  await settle();
  assert.ok(
    labelled(
      stage.frames.flatMap((frame) => frame.patches),
      'Volume cache was removed.',
    ),
  );
});

test('confirmation flow is selectable from the shipped playground', () => {
  const stage = host();
  stage.render(h(Playground, { initialStory: CONFIRMATION_STORY }));
  const patches = stage.frames.flatMap((frame) => frame.patches);
  assert.ok(labelled(patches, CONFIRMATION_STORY));
  assert.ok(labelled(patches, 'Remove volume'));
});

test('ConfirmAction documents compact, default, and touch-forward sizes on its own page', () => {
  const frame = host().render(h(ConfirmationStory));
  const labels = frame.patches
    .filter((patch) => patch.SetProp?.prop === 'Label')
    .map((patch) => patch.SetProp.value?.Text);
  const sizes = Object.fromEntries(
    frame.patches
      .filter((patch) => patch.SetProp?.prop === 'Size')
      .map((patch) => {
        const label = frame.patches.find(
          (candidate) =>
            candidate.SetProp?.id === patch.SetProp.id && candidate.SetProp.prop === 'Label',
        )?.SetProp.value?.Text;
        return [label, patch.SetProp.value?.ControlSize];
      }),
  );

  assert(labels.includes('Destructive flow'));
  assert(labels.includes('Sizes'));
  assert.deepEqual(sizes, { Small: 'Small', Medium: 'Medium', Large: 'Large' });
  assert.equal(
    frame.patches.filter((patch) => patch.Create?.tag === 'ConfirmAction').length,
    0,
    'the composite resolves to protocol-native controls rather than inventing a host tag',
  );
});
