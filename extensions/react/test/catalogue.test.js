import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import test from 'node:test';
import { fileURLToPath } from 'node:url';

import * as exported from '../src/components.js';
import { tags } from '../src/components.js';
import { vocabulary } from '../src/index.js';

const here = path.dirname(fileURLToPath(import.meta.url));
const tagSource = path.resolve(here, '../../../src/workspaces/hl-gui/src/node/tag.rs');

test('every tag in the catalogue is exported by name', () => {
  for (const tag of tags) {
    assert.equal(exported[tag], tag, `<${tag}> is missing from components.js`);
  }
  assert.ok(tags.length >= 120, 'the catalogue is the whole component library');
});

test('the catalogue still matches the Rust vocabulary', (t) => {
  if (!fs.existsSync(tagSource)) return t.skip('the Rust tree is not beside this package');
  const source = fs.readFileSync(tagSource, 'utf8');
  const body = source.slice(source.indexOf('catalogue! {'));
  const declared = [...body.matchAll(/^ {4}([A-Z][A-Za-z]*): /gm)].map((match) => match[1]);
  assert.deepEqual(tags, declared);
});

test('the property vocabulary covers the whole Prop enum', (t) => {
  const propSource = path.resolve(here, '../../../src/workspaces/hl-gui/src/node/prop.rs');
  if (!fs.existsSync(propSource)) return t.skip('the Rust tree is not beside this package');
  const source = fs.readFileSync(propSource, 'utf8');
  const body = source.slice(source.indexOf('pub enum Prop {'), source.indexOf('/// Orientation of a container'));
  const declared = [...body.matchAll(/^ {4}([A-Z][A-Za-z]*),$/gm)].map((match) => match[1][0].toLowerCase() + match[1].slice(1));
  assert.deepEqual([...vocabulary.props].sort(), declared.sort());
});
