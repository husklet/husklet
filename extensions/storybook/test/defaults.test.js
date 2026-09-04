import assert from 'node:assert/strict';
import test from 'node:test';

import { component, tags } from '../dist/catalogue.js';
import { all, defaults } from '../dist/defaults.js';

test('selecting a component produces a default set of the right shape', () => {
  for (const [name, opened] of all()) {
    const tag = component(name);
    assert.equal(typeof opened.props, 'object');
    assert.ok(Array.isArray(opened.children));
    assert.equal(
      opened.children.length > 0,
      tag.acceptsChildren && opened.children.length > 0,
      `<${name}> was given children it cannot hold`,
    );
    if (!tag.acceptsChildren) assert.deepEqual(opened.children, [], `<${name}> is a leaf`);
  }
});

test('no default preview is blank', () => {
  for (const tag of tags) {
    const opened = defaults(tag.name);
    const visible = Object.keys(opened.props).length > 0 || opened.children.length > 0;
    assert.ok(visible, `<${tag.name}> would open as an empty preview`);
  }
});

test('a leaf carries something to show', () => {
  for (const tag of tags.filter((entry) => !entry.acceptsChildren)) {
    const { props } = defaults(tag.name);
    const shows = ['label', 'icon', 'uri', 'value', 'fraction', 'checked', 'busy', 'width', 'height', 'choices'];
    assert.ok(shows.some((name) => props[name] !== undefined), `<${tag.name}> opens with nothing to see`);
  }
});

test('defaults are fresh each time, so editing one selection cannot leak into another', () => {
  const first = defaults('Button');
  first.props.label = 'changed';
  assert.notEqual(defaults('Button').props.label, 'changed');
});
