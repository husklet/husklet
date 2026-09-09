import assert from 'node:assert/strict';
import test from 'node:test';
import reactCatalogue from '../../../extensions/base/react/catalogue.json' with { type: 'json' };

import { components } from '@husklet/react';

import catalogue, {
  SHAPE_VERSION,
  component,
  enums,
  families,
  grouped,
  props,
  tags,
} from '../dist/catalogue.js';

test('the catalogue describes the whole library', () => {
  assert.equal(catalogue.version, SHAPE_VERSION);
  assert.ok(
    tags.length >= 120,
    `only ${tags.length} components; the catalogue is the whole library`,
  );
  assert.equal(props.length, 46);
  assert.ok(families.length > 0);
});

test('the generated catalogue carries semantic absence defaults', () => {
  const defaults = Object.fromEntries(props.map((prop) => [prop.name, prop.default]));
  assert.equal(defaults.Enabled, 'true');
  assert.equal(defaults.Checked, 'false');
  assert.equal(defaults.Variant, 'plain');
  assert.equal(defaults.Tone, 'neutral');
  assert.equal(defaults.Size, 'small');
  assert.equal(defaults.Label, null);
});

test('every component in the catalogue is constructible', () => {
  for (const tag of tags) {
    assert.equal(
      components[tag.name],
      tag.name,
      `<${tag.name}> is not a component of @husklet/react`,
    );
  }
});

test('the checked-in catalogue matches the React binding catalogue', () => {
  assert.deepEqual(catalogue, reactCatalogue);
});

test('every tag declares its property and interaction contract', () => {
  for (const tag of tags) {
    assert.ok(Array.isArray(tag.props), `<${tag.name}> has no property contract`);
    assert.ok(
      tag.propNotes && typeof tag.propNotes === 'object',
      `<${tag.name}> has no prop notes`,
    );
    for (const name of Object.keys(tag.propNotes)) {
      assert.ok(tag.props.includes(name), `<${tag.name}> documents undeclared ${name}`);
    }
    assert.equal(typeof tag.propConstraints, 'object');
    for (const [name, constraint] of Object.entries(tag.propConstraints)) {
      assert.ok(tag.props.includes(name), `<${tag.name}> constrains undeclared ${name}`);
      assert.equal(constraint, 'non-empty');
    }
    assert.ok(Array.isArray(tag.triggers), `<${tag.name}> has no interaction contract`);
  }
});

test('IconButton publishes its accessible identity requirements', () => {
  assert.deepEqual(component('IconButton').propConstraints, {
    Icon: 'non-empty',
    Label: 'non-empty',
  });
});

test('core controls override transport vocabulary with component-specific contracts', () => {
  const note = (tag, prop) => tags.find((candidate) => candidate.name === tag).propNotes[prop];
  assert.match(note('Button', 'Variant'), /primary action/);
  assert.match(note('IconButton', 'Label'), /accessible action name/);
  assert.match(note('Entry', 'Value'), /complete new string/);
  assert.match(note('Select', 'Choices'), /stable identities/);
  assert.match(note('Switch', 'Selected'), /Checked is absent/);
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
  assert.deepEqual(seen.slice().sort(), tags.map((tag) => tag.name).sort());
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
