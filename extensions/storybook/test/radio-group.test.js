import assert from 'node:assert/strict';
import test from 'node:test';
import { createElement as h } from 'react';

import { RadioGroupWorkbench } from '../dist/radio-group.js';
import { rows } from '../dist/editors.js';
import { host } from './host.js';

function labels(patches) {
  return patches
    .filter((patch) => patch.SetProp?.prop === 'Label')
    .map((patch) => patch.SetProp.value?.Text);
}

test('RadioGroup coordinates authored children through one controlled selection', () => {
  const stage = host();
  const first = stage.render(h(RadioGroupWorkbench));
  const radios = first.patches
    .filter((patch) => patch.Create?.tag === 'Radio')
    .map((patch) => patch.Create.id);
  assert.equal(radios.length, 7);
  const before = stage.frames.length;
  assert(
    stage.surface.dispatch({
      trigger: 'Toggle',
      node: radios[2],
      id: `${radios[2]}:Toggle`,
      value: true,
    }),
  );
  const changed = stage.since(before);
  assert(labels(changed).includes('Selected channel: nightly'));
  const states = new Map(
    [...first.patches, ...changed]
      .filter(
        (patch) =>
          radios.slice(0, 3).includes(patch.SetProp?.id) && patch.SetProp?.prop === 'Checked',
      )
      .map((patch) => [patch.SetProp.id, patch.SetProp.value?.Flag]),
  );
  assert.deepEqual([...states.values()], [false, false, true]);
});

test('RadioGroup documents real child ownership, layouts, and keyboard behavior before API', () => {
  const frame = host().render(h(RadioGroupWorkbench));
  const text = labels(frame.patches);
  for (const label of [
    'Vertical',
    'Horizontal',
    'Ownership and events',
    'Keyboard and accessibility',
  ]) {
    assert(text.includes(label), `missing ${label}`);
  }
  assert(text.some((label) => label?.includes('skips disabled children')));
  assert(text.some((label) => label?.includes('does not accept choices')));
  assert(text.indexOf('Layout') < text.indexOf('API'));
  assert(text.indexOf('Inputs') < text.indexOf('API'));
});

test('RadioGroup public API rejects the inert choices shortcut', () => {
  assert(!rows('RadioGroup').some((row) => row.name === 'choices'));
});
