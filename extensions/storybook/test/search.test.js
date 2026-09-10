import assert from 'node:assert/strict';
import test from 'node:test';
import { createElement as h } from 'react';

import { componentPages } from '../dist/component-pages.js';
import { SearchWorkbench } from '../dist/search.js';
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

test('Search owns a focused one-component reference with every genuine state', () => {
  assert.equal(componentPages.Search, SearchWorkbench);
  const frame = host().render(h(SearchWorkbench));
  const headings = creations(frame, 'Heading').map((id) => properties(frame, id).Label?.Text);
  assert.deepEqual(headings.slice(0, 6), [
    'Search',
    'Overview',
    'States',
    'Behavior',
    'Accessibility',
    'API',
  ]);
  const searches = creations(frame, 'Search').map((id) => properties(frame, id));
  assert.equal(searches.length, 5);
  assert(searches.some((props) => props.Value?.Text === ''));
  assert(searches.some((props) => props.Enabled?.Flag === false));
  assert(searches.some((props) => props.Tooltip?.Text === 'Focused extension search'));
});

test('Search query changes update both the controlled field and visible feedback', () => {
  const stage = host();
  const first = stage.render(h(SearchWorkbench));
  const search = creations(first, 'Search')[0];
  const before = stage.frames.length;
  assert(
    stage.surface.dispatch({ trigger: 'Change', node: search, id: `${search}:Change`, value: '' }),
  );
  const patches = stage.since(before);
  assert(
    patches.some((patch) => patch.SetProp?.prop === 'Value' && patch.SetProp.value.Text === ''),
  );
  assert(patches.some((patch) => patch.SetProp?.value?.Text === 'Showing every extension.'));
});
