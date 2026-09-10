import assert from 'node:assert/strict';
import test from 'node:test';
import { createElement as h } from 'react';
import { AutocompleteWorkbench } from '../dist/autocomplete.js';
import { componentPages } from '../dist/component-pages.js';
import { host } from './host.js';

const creations = (frame, tag) =>
  frame.patches.filter((patch) => patch.Create?.tag === tag).map((patch) => patch.Create.id);
const properties = (frame, id) =>
  Object.fromEntries(
    frame.patches
      .filter((patch) => patch.SetProp?.id === id)
      .map((patch) => [patch.SetProp.prop, patch.SetProp.value]),
  );

test('Autocomplete owns a focused fixed-choice reference', () => {
  assert.equal(componentPages.Autocomplete, AutocompleteWorkbench);
  const frame = host().render(h(AutocompleteWorkbench));
  const headings = creations(frame, 'Heading').map((id) => properties(frame, id).Label?.Text);
  assert.deepEqual(headings.slice(0, 7), [
    'Autocomplete',
    'Overview',
    'Options',
    'States',
    'Behavior',
    'Accessibility',
    'API',
  ]);
  const controls = creations(frame, 'Autocomplete').map((id) => properties(frame, id));
  assert.equal(controls.length, 4);
  assert(controls.some((props) => props.Choices?.Choices?.length === 0));
  assert(controls.some((props) => props.Enabled?.Flag === false));
});

test('Autocomplete selection produces stable visible feedback', () => {
  const stage = host();
  const frame = stage.render(h(AutocompleteWorkbench));
  const control = creations(frame, 'Autocomplete')[0];
  const before = stage.frames.length;
  assert(
    stage.surface.dispatch({
      trigger: 'Select',
      node: control,
      id: `${control}:Select`,
      rows: [1],
    }),
  );
  assert(
    stage.since(before).some((patch) => patch.SetProp?.value?.Text === 'Selected Python 3.13.'),
  );
});
