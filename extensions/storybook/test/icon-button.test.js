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
});
