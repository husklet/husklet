import assert from 'node:assert/strict';
import test from 'node:test';
import { createElement as h } from 'react';

import { HeadingWorkbench } from '../dist/heading.js';
import { host } from './host.js';

function properties(patches, tag) {
  const ids = new Set(
    patches.filter((patch) => patch.Create?.tag === tag).map((patch) => patch.Create.id),
  );
  const props = new Map();
  for (const patch of patches) {
    if (!patch.SetProp || !ids.has(patch.SetProp.id)) continue;
    const entry = props.get(patch.SetProp.id) ?? {};
    entry[patch.SetProp.prop] = patch.SetProp.value;
    props.set(patch.SetProp.id, entry);
  }
  return [...props.values()];
}

test('Heading owns a precise single-component type-scale document', () => {
  const frame = host().render(h(HeadingWorkbench));
  const headings = properties(frame.patches, 'Heading');
  const labels = headings.map((props) => props.Label?.Text);
  for (const section of [
    'Heading',
    'Overview',
    'Type scale',
    'Wrapping and truncation',
    'Accessibility',
    'API',
  ])
    assert(labels.includes(section), `Heading document is missing ${section}`);
  assert.deepEqual(
    headings
      .filter((props) => props.Label?.Text === 'Build, inspect, and ship with confidence')
      .map((props) => props.Scale),
    [{ Scale: 'Caption' }, { Scale: 'Body' }, { Scale: 'Title' }, { Scale: 'Display' }],
  );
  const text = frame.patches
    .filter((patch) => patch.SetProp?.prop === 'Label')
    .map((patch) => patch.SetProp.value?.Text);
  for (const metric of [
    'caption · 12px · weight 450',
    'body · 14px · weight 400',
    'title · 18px · weight 600',
    'display · 24px · weight 700',
  ])
    assert(text.includes(metric), `Heading document is missing generated metric ${metric}`);
  assert(frame.patches.some((patch) => patch.Create?.tag === 'Table'));
  assert(frame.patches.some((patch) => patch.SetProp?.value?.Text === 'Playground'));
});
