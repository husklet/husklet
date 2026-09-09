import assert from 'node:assert/strict';
import test from 'node:test';
import { createElement as h } from 'react';

import { ButtonWorkbench } from '../dist/button.js';
import { host } from './host.js';

function propsFor(patches, tag) {
  const ids = patches.filter((patch) => patch.Create?.tag === tag).map((patch) => patch.Create.id);
  return ids.map((id) =>
    Object.fromEntries(
      patches
        .filter((patch) => patch.SetProp?.id === id)
        .map((patch) => [patch.SetProp.prop, patch.SetProp.value]),
    ),
  );
}

test('Button documents every semantic size, variant, and tone as live controls', () => {
  const frame = host().render(h(ButtonWorkbench));
  const buttons = propsFor(frame.patches, 'Button');

  for (const size of ['Small', 'Medium', 'Large']) {
    assert(
      buttons.some((props) => props.Size?.ControlSize === size),
      `missing ${size} size`,
    );
  }
  for (const variant of ['Filled', 'Outline', 'Ghost', 'Plain']) {
    assert(
      buttons.some((props) => props.Variant?.Variant === variant),
      `missing ${variant}`,
    );
  }
  for (const tone of ['Neutral', 'Accent', 'Positive', 'Warning', 'Danger']) {
    assert(
      buttons.some((props) => props.Tone?.Tone === tone),
      `missing ${tone}`,
    );
  }
  assert.equal(
    propsFor(frame.patches, 'IconButton').length,
    0,
    'IconButton belongs on its own page',
  );
});

test('Button puts its API beside the overview and keeps the playground secondary', () => {
  const frame = host().render(h(ButtonWorkbench));
  const headings = propsFor(frame.patches, 'Heading').map((props) => props.Label?.Text);
  const expanders = propsFor(frame.patches, 'Expander').map((props) => props.Label?.Text);

  assert.deepEqual(headings.slice(0, 3), ['Button', 'Overview', 'API']);
  assert(expanders.includes('Playground'));
});
