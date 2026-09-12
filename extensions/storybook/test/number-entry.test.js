import assert from 'node:assert/strict';
import test from 'node:test';
import { createElement as h } from 'react';
import { componentPages } from '../dist/component-pages.js';
import { NumberEntryWorkbench } from '../dist/number-entry.js';
import { host } from './host.js';

const creations = (frame, tag) =>
  frame.patches.filter((patch) => patch.Create?.tag === tag).map((patch) => patch.Create.id);
const properties = (frame, id) =>
  Object.fromEntries(
    frame.patches
      .filter((patch) => patch.SetProp?.id === id)
      .map((patch) => [patch.SetProp.prop, patch.SetProp.value]),
  );

test('NumberEntry documents bounds, steps, and availability as live controls', () => {
  assert.equal(componentPages.NumberEntry, NumberEntryWorkbench);
  const frame = host().render(h(NumberEntryWorkbench));
  const headings = creations(frame, 'Heading').map((id) => properties(frame, id).Label?.Text);
  assert.deepEqual(headings.slice(0, 8), [
    'NumberEntry',
    'Overview',
    'Bounds',
    'Steps',
    'States',
    'Behavior',
    'Accessibility',
    'API',
  ]);
  const controls = creations(frame, 'NumberEntry').map((id) => properties(frame, id));
  assert.equal(controls.length, 8);
  assert(controls.some((props) => props.Step?.Number === 0.25));
  assert(controls.some((props) => props.Enabled?.Flag === false));
  assert(controls.some((props) => props.Tone?.Tone === 'Danger'));
});

test('NumberEntry reports and retains a numeric controlled value', () => {
  const stage = host();
  const frame = stage.render(h(NumberEntryWorkbench));
  const control = creations(frame, 'NumberEntry')[0];
  const before = stage.frames.length;
  assert(
    stage.surface.dispatch({ trigger: 'Change', node: control, id: `${control}:Change`, value: 6 }),
  );
  const patches = stage.since(before);
  assert(
    patches.some(
      (patch) =>
        patch.SetProp?.id === control &&
        patch.SetProp.prop === 'Value' &&
        (patch.SetProp.value.Integer === 6 || patch.SetProp.value.Number === 6),
    ),
  );
  assert(patches.some((patch) => patch.SetProp?.value?.Text === '6 concurrent workers'));
});
