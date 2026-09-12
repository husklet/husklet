import assert from 'node:assert/strict';
import test from 'node:test';
import { createElement as h } from 'react';

import { componentPages } from '../dist/component-pages.js';
import { InlineButtonWorkbench } from '../dist/inline-button.js';
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

test('InlineButton has an authored single-component page with variants and states', () => {
  assert.equal(componentPages.InlineButton, InlineButtonWorkbench);
  const frame = host().render(h(InlineButtonWorkbench));
  const actions = propsFor(frame.patches, 'InlineButton');

  for (const variant of ['Outline', 'Filled', 'Ghost', 'Plain']) {
    assert.ok(
      actions.some((props) => props.Variant?.Variant === variant),
      `missing ${variant} specimen`,
    );
  }
  assert.ok(
    actions.some((props) => props.Enabled?.Flag === false),
    'missing disabled specimen',
  );
  assert.ok(
    actions.some((props) => props.Busy?.Flag === true),
    'missing busy specimen',
  );
  assert.equal(
    propsFor(frame.patches, 'Button').length,
    0,
    'the page demonstrates InlineButton without substituting a standard Button',
  );
  for (const label of ['Focus filled', 'Focus outline', 'Focus ghost']) {
    assert.ok(
      actions.some((props) => props.Label?.Text === label),
      `missing ${label} state`,
    );
  }
});

test('InlineButton teaches compact chrome and full target semantics before its API', () => {
  const frame = host().render(h(InlineButtonWorkbench));
  const labels = propsFor(frame.patches, 'Text').map((props) => props.Label?.Text);
  const headings = propsFor(frame.patches, 'Heading').map((props) => props.Label?.Text);

  assert.ok(labels.some((label) => label?.includes('visible chrome is 28px')));
  assert.ok(labels.some((label) => label?.includes('hit target remains at least 44px')));
  assert.ok(labels.some((label) => label?.includes('one accent ring around the compact chrome')));
  assert.deepEqual(headings.slice(0, 3), ['InlineButton', 'Overview', 'Variants']);
  assert.ok(headings.indexOf('Accessibility') < headings.indexOf('API'));
});
