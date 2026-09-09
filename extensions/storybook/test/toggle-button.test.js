import assert from 'node:assert/strict';
import test from 'node:test';
import { createElement as h } from 'react';

import { ToggleButtonWorkbench } from '../dist/toggle-button.js';
import { host } from './host.js';

function created(patches, tag) {
  return patches.filter((patch) => patch.Create?.tag === tag).map((patch) => patch.Create.id);
}

function propsFor(patches, tag) {
  return created(patches, tag).map((id) =>
    Object.fromEntries(
      patches
        .filter((patch) => patch.SetProp?.id === id)
        .map((patch) => [patch.SetProp.prop, patch.SetProp.value]),
    ),
  );
}

test('ToggleButton teaches persistent selected and unselected semantics', () => {
  const frame = host().render(h(ToggleButtonWorkbench));
  const headings = propsFor(frame.patches, 'Heading').map((props) => props.Label?.Text);
  const toggles = propsFor(frame.patches, 'ToggleButton');
  const text = propsFor(frame.patches, 'Text').map((props) => props.Label?.Text);

  assert.deepEqual(headings.slice(0, 3), ['Toggle Button', 'Overview', 'Selected and unselected']);
  assert(toggles.some((props) => props.Checked?.Flag === true));
  assert(toggles.some((props) => props.Checked?.Flag === false));
  assert(text.some((label) => label?.includes('pressed')));
  assert(text.some((label) => label?.includes('ToggleButtonGroup')));
});

test('ToggleButton reports a meaningful controlled state transition', () => {
  const stage = host();
  const first = stage.render(h(ToggleButtonWorkbench));
  const toggle = created(first.patches, 'ToggleButton')[0];
  assert.ok(toggle);

  const before = stage.frames.length;
  assert.ok(
    stage.surface.dispatch({
      trigger: 'Toggle',
      node: toggle,
      id: `${toggle}:Toggle`,
      value: false,
    }),
  );
  const patches = stage.since(before);
  assert(patches.some((patch) => patch.SetProp?.value?.Text === 'Pin tab changed to unselected.'));
  assert(
    patches.some(
      (patch) => patch.SetProp?.prop === 'Checked' && patch.SetProp.value.Flag === false,
    ),
  );
});
