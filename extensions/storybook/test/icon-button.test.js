import assert from 'node:assert/strict';
import test from 'node:test';
import { createElement as h } from 'react';

import { IconButtonWorkbench } from '../dist/icon-button.js';
import { host } from './host.js';

test('IconButton owns a focused document with square semantic sizes and accessible labels', () => {
  const frame = host().render(h(IconButtonWorkbench));
  const ids = frame.patches
    .filter((patch) => patch.Create?.tag === 'IconButton')
    .map((patch) => patch.Create.id);
  const props = ids.map((id) =>
    Object.fromEntries(
      frame.patches
        .filter((patch) => patch.SetProp?.id === id)
        .map((patch) => [patch.SetProp.prop, patch.SetProp.value]),
    ),
  );

  for (const size of ['Small', 'Medium', 'Large']) {
    assert(
      props.some((value) => value.Size?.ControlSize === size),
      `missing ${size} size`,
    );
  }
  for (const variant of ['Filled', 'Outline', 'Ghost', 'Plain']) {
    assert(
      props.some((value) => value.Variant?.Variant === variant),
      `missing ${variant} variant`,
    );
  }
  assert(
    props.every((value) => value.Label?.Text),
    'an icon-only action has no accessible label',
  );
  assert(
    props.every((value) => value.Tooltip?.Text),
    'an icon-only action has no tooltip',
  );
  assert.equal(
    frame.patches.filter((patch) => patch.Create?.tag === 'Button').length,
    0,
    'Button examples belong on the Button page',
  );
  const cellLabels = frame.patches
    .filter((patch) => patch.SetProp?.prop === 'Label')
    .map((patch) => patch.SetProp.value?.Text);
  for (const heading of ['Property', 'Type', 'Default', 'Description']) {
    assert(cellLabels.includes(heading), `IconButton API is missing ${heading}`);
  }
  const headings = cellLabels.filter((label) =>
    ['IconButton', 'Overview', 'Sizes', 'Variants', 'States', 'Accessibility', 'API'].includes(
      label,
    ),
  );
  assert(headings.indexOf('Overview') < headings.indexOf('Sizes'));
  assert(headings.indexOf('States') < headings.indexOf('API'));
  assert(cellLabels.includes('Focus'));
  assert(cellLabels.includes('Pressed'));
  assert(cellLabels.includes('Toolbar action'));
  assert(cellLabels.includes('Refresh containers'));
  assert(
    props.some(
      (value) =>
        value.Label?.Text === 'Refresh containers' &&
        value.Size?.ControlSize === 'Small' &&
        value.Variant?.Variant === 'Ghost',
    ),
    'the page-heading specimen uses a compact secondary icon action',
  );
});
