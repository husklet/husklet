import assert from 'node:assert/strict';
import test from 'node:test';
import reactCatalogue from '../../react/catalogue.json' with { type: 'json' };

import { components } from '@husklet/react';

import catalogue, { enums, families, grouped, props, tags } from '../src/catalogue.js';

test('the catalogue describes the whole library', () => {
  assert.equal(catalogue.version, 1);
  assert.ok(tags.length >= 120, `only ${tags.length} components; the catalogue is the whole library`);
  assert.equal(props.length, 42);
  assert.ok(families.length > 0);
});

test('every component in the catalogue is constructible', () => {
  for (const tag of tags) {
    assert.equal(components[tag.name], tag.name, `<${tag.name}> is not a component of @husklet/react`);
  }
});

test('the checked-in catalogue matches the React binding catalogue', () => {
  assert.deepEqual(catalogue, reactCatalogue);
});

test('every tag declares its property and interaction contract', () => {
  for (const tag of tags) {
    assert.ok(Array.isArray(tag.props), `<${tag.name}> has no property contract`);
    assert.ok(Array.isArray(tag.triggers), `<${tag.name}> has no interaction contract`);
  }
});

test('the sidebar covers every component exactly once', () => {
  const seen = [];
  for (const family of grouped()) {
    assert.ok(family.label, `family ${family.name} has no label to head its group with`);
    for (const tag of family.tags) {
      assert.equal(tag.family, family.name);
      seen.push(tag.name);
    }
  }
  assert.deepEqual(
    seen.slice().sort(),
    tags.map((tag) => tag.name).sort(),
  );
  assert.equal(new Set(seen).size, seen.length, 'a component appears under two families');
  assert.equal(seen.length, tags.length);
});

test('every enum property names a vocabulary the catalogue spells out', () => {
  for (const prop of props.filter((entry) => entry.editor === 'enum')) {
    const vocabulary = prop.values.find((name) => name in enums);
    assert.ok(vocabulary, `${prop.name} is an enum with no members`);
    assert.ok(enums[vocabulary].length > 0);
  }
});
