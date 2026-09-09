import assert from 'node:assert/strict';
import test from 'node:test';
import { createElement as h } from 'react';

import { RadioWorkbench } from '../dist/radio.js';
import { host } from './host.js';

function labels(patches) {
  return patches
    .filter((patch) => patch.SetProp?.prop === 'Label')
    .map((patch) => patch.SetProp.value?.Text);
}

test('Radio group retains exactly one controlled selection and result', () => {
  const stage = host();
  const first = stage.render(h(RadioWorkbench));
  const radios = first.patches
    .filter((patch) => patch.Create?.tag === 'Radio')
    .map((patch) => patch.Create.id);
  assert.equal(radios.length, 7);
  const before = stage.frames.length;
  assert(
    stage.surface.dispatch({
      trigger: 'Toggle',
      node: radios[1],
      id: `${radios[1]}:Toggle`,
      value: true,
    }),
  );
  const changed = stage.since(before);
  assert(labels(changed).includes('Selected shell: bash'));
  const states = new Map(
    [...first.patches, ...changed]
      .filter(
        (patch) =>
          radios.slice(0, 3).includes(patch.SetProp?.id) && patch.SetProp?.prop === 'Checked',
      )
      .map((patch) => [patch.SetProp.id, patch.SetProp.value?.Flag]),
  );
  assert.deepEqual([...states.values()], [false, true, false]);
});

test('Radio documents native grouping, disabled states, aliases, and invalid standalone use before API', () => {
  const frame = host().render(h(RadioWorkbench));
  const text = labels(frame.patches);
  for (const label of [
    'Enabled group',
    'Disabled group',
    'Managed · selected',
    'Custom · unselected',
  ]) {
    assert(text.includes(label), `missing ${label}`);
  }
  assert(text.some((label) => label?.includes('Arrow keys')));
  assert(text.some((label) => label?.includes('legacy alias')));
  assert(text.some((label) => label?.includes('standalone Radio is invalid')));
  assert(text.some((label) => label?.includes('Checkbox')));
  assert(text.indexOf('States') < text.indexOf('API'));
  assert(text.indexOf('Keyboard and accessibility') < text.indexOf('API'));
});
