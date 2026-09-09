import assert from 'node:assert/strict';
import test from 'node:test';
import { createElement as h } from 'react';

import { CheckboxWorkbench } from '../dist/checkbox.js';
import { host } from './host.js';

function labels(patches) {
  return patches
    .filter((patch) => patch.SetProp?.prop === 'Label')
    .map((patch) => patch.SetProp.value?.Text);
}

test('Checkbox is controlled and reports its checked result', () => {
  const stage = host();
  const first = stage.render(h(CheckboxWorkbench));
  const checkbox = first.patches.find((patch) => patch.Create?.tag === 'Checkbox')?.Create.id;
  assert.ok(checkbox);
  const before = stage.frames.length;
  assert(
    stage.surface.dispatch({
      trigger: 'Toggle',
      node: checkbox,
      id: `${checkbox}:Toggle`,
      value: false,
    }),
  );
  const changed = stage.since(before);
  assert(
    changed.some(
      (patch) => patch.SetProp?.prop === 'Checked' && patch.SetProp.value?.Flag === false,
    ),
  );
  assert(labels(changed).includes('Include diagnostics is unchecked.'));
});

test('Checkbox teaches its state matrix and keyboard semantics before API', () => {
  const frame = host().render(h(CheckboxWorkbench));
  const text = labels(frame.patches);
  for (const state of [
    'Enabled · unchecked',
    'Enabled · checked',
    'Disabled · unchecked',
    'Disabled · checked',
  ]) {
    assert(text.includes(state), `missing ${state}`);
  }
  assert(text.some((label) => label?.includes('Space toggles')));
  assert(text.some((label) => label?.includes('Switch')));
  assert(text.some((label) => label?.includes('Radio')));
  assert(text.indexOf('States') < text.indexOf('API'));
  assert(text.indexOf('Keyboard and accessibility') < text.indexOf('API'));
});
