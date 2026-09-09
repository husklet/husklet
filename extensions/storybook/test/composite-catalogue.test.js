import assert from 'node:assert/strict';
import test from 'node:test';
import { createElement as h } from 'react';

import { compositeComponents } from '@husklet/react';
import { FLOW_STORIES, Playground } from '../dist/app.js';
import { componentPages } from '../dist/component-pages.js';
import { grouped, tags } from '../dist/catalogue.js';
import { host } from './host.js';

const names = compositeComponents.map(({ name }) => name);

test('every public React composite has exactly one component route and catalogue entry', () => {
  assert.equal(new Set(names).size, names.length);
  for (const name of names) {
    assert.equal(tags.filter((tag) => tag.name === name).length, 1, `${name} catalogue entries`);
    assert.equal(typeof componentPages[name], 'function', `${name} has no page`);
    assert.equal(FLOW_STORIES.includes(name), false, `${name} is still classified as a workflow`);
  }
  const navigated = grouped().flatMap((family) => family.tags.map(({ name }) => name));
  for (const name of names) assert.equal(navigated.filter((entry) => entry === name).length, 1);
});

test('each composite route renders only its selected component document', () => {
  for (const selected of names) {
    const frame = host().render(h(Playground, { initialStory: selected }));
    const headingIds = new Set(
      frame.patches
        .filter((patch) => patch.Create?.tag === 'Heading')
        .map((patch) => patch.Create.id),
    );
    const headings = frame.patches
      .filter((patch) => headingIds.has(patch.SetProp?.id) && patch.SetProp?.prop === 'Label')
      .map((patch) => patch.SetProp.value?.Text);
    assert.equal(
      headings.filter((label) => label === selected).length,
      1,
      `${selected} is not one isolated document`,
    );
    for (const sibling of names.filter((name) => name !== selected)) {
      assert.equal(
        headings.includes(sibling),
        false,
        `${selected} materialized sibling ${sibling}`,
      );
    }
  }
});
