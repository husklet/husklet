import assert from 'node:assert/strict';
import test from 'node:test';
import { createElement as h } from 'react';

import { componentPages } from '../dist/component-pages.js';
import { TextAreaWorkbench } from '../dist/text-area.js';
import { host } from './host.js';

function creations(frame, tag) {
  return frame.patches.filter((patch) => patch.Create?.tag === tag).map((patch) => patch.Create.id);
}

function properties(frame, id) {
  return Object.fromEntries(
    frame.patches
      .filter((patch) => patch.SetProp?.id === id)
      .map((patch) => [patch.SetProp.prop, patch.SetProp.value]),
  );
}

test('TextArea owns a focused multi-line component reference', () => {
  assert.equal(componentPages.TextArea, TextAreaWorkbench);
  const frame = host().render(h(TextAreaWorkbench));
  const headings = creations(frame, 'Heading').map((id) => properties(frame, id).Label?.Text);
  assert.deepEqual(headings.slice(0, 7), [
    'TextArea',
    'Overview',
    'Presentation',
    'States',
    'Behavior',
    'Accessibility',
    'API',
  ]);
  const editors = creations(frame, 'TextArea').map((id) => properties(frame, id));
  assert.equal(editors.length, 6);
  assert(editors.some((props) => props.Monospace?.Flag === false));
  assert(editors.some((props) => props.Enabled?.Flag === false));
  assert(editors.some((props) => props.Tone?.Tone === 'Danger'));
});

test('TextArea retains its controlled multi-line Change value and visible count', () => {
  const stage = host();
  const first = stage.render(h(TextAreaWorkbench));
  const editor = creations(first, 'TextArea')[0];
  const before = stage.frames.length;
  assert(
    stage.surface.dispatch({
      trigger: 'Change',
      node: editor,
      id: `${editor}:Change`,
      value: 'one\ntwo\nthree',
    }),
  );
  const patches = stage.since(before);
  assert(
    patches.some(
      (patch) =>
        patch.SetProp?.id === editor &&
        patch.SetProp.prop === 'Value' &&
        patch.SetProp.value.Text === 'one\ntwo\nthree',
    ),
  );
  assert(patches.some((patch) => patch.SetProp?.value?.Text === '3 lines · 13 characters'));
});
