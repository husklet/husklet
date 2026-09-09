import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import test from 'node:test';
import { fileURLToPath } from 'node:url';

import * as exported from '../dist/components.js';
import { tags } from '../dist/components.js';
import {
  LOG_VIEW_CHARACTER_LIMIT,
  compositeComponents,
  vocabulary,
} from '../dist/index.js';
import * as publicApi from '../dist/index.js';

const here = path.dirname(fileURLToPath(import.meta.url));
const tagSource = path.resolve(here, '../../../../dist/workspaces/hl-gui/src/node/tag.rs');

test('every tag in the catalogue is exported by name', () => {
  for (const tag of tags) {
    assert.equal(exported[tag], tag, `<${tag}> is missing from components.js`);
  }
  assert.ok(tags.length >= 120, 'the catalogue is the whole component library');
});

test('LogView publishes its bounded append-only retention contract', () => {
  const catalogue = JSON.parse(fs.readFileSync(path.resolve(here, '../catalogue.json'), 'utf8'));
  assert.equal(LOG_VIEW_CHARACTER_LIMIT, 4096);
  assert.match(catalogue.notes.logViewRetention, /append.*newest 4096 Unicode characters/i);
});

test('every public composite publishes one documentation identity beside its export', () => {
  const names = compositeComponents.map(({ name }) => name);
  assert.equal(new Set(names).size, names.length);
  for (const definition of compositeComponents) {
    assert.equal(typeof publicApi[definition.name], 'function', `${definition.name} is not exported`);
    assert.notEqual(definition.summary.trim(), '');
  }
});

test('the catalogue still matches the Rust vocabulary', (t) => {
  if (!fs.existsSync(tagSource)) return t.skip('the Rust tree is not beside this package');
  const source = fs.readFileSync(tagSource, 'utf8');
  const body = source.slice(source.indexOf('catalogue! {'));
  const declared = [...body.matchAll(/^ {4}([A-Z][A-Za-z]*): /gm)].map((match) => match[1]);
  assert.deepEqual(tags, declared);
});

test('the property vocabulary covers the whole Prop enum', (t) => {
  const propSource = path.resolve(here, '../../../../dist/workspaces/hl-gui/src/node/prop.rs');
  if (!fs.existsSync(propSource)) return t.skip('the Rust tree is not beside this package');
  const source = fs.readFileSync(propSource, 'utf8');
  const body = source.slice(source.indexOf('pub enum Prop {'), source.indexOf('/// Orientation of a container'));
  const declared = [...body.matchAll(/^ {4}([A-Z][A-Za-z]*),$/gm)].map((match) => match[1][0].toLowerCase() + match[1].slice(1));
  assert.deepEqual([...vocabulary.props].sort(), declared.sort());
});
