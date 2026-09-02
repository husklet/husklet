import assert from 'node:assert/strict';
import test from 'node:test';

import { props } from '../src/catalogue.js';
import { BINDING, control, editorOf } from '../src/editors.js';
import { value } from './host.js';

/** What a control of each kind hands its property. */
const SAMPLE = {
  text: 'text',
  switch: true,
  enum: null,
  number: 1,
  length: 1,
  edges: 1,
  choices: [{ value: 'one', label: 'One' }],
  schema: [{ key: 'name' }],
  source: 1,
};

/** The control that matches a wire shape, when the catalogue's hint does not. */
const CONTROL = { Flag: 'switch', Text: 'text', Number: 'number', Integer: 'number', Length: 'length' };

/**
 * The React binding decides a property's wire shape, and the catalogue says
 * which shapes the library reads for it. Where the two disagree the playground
 * follows the binding — and the disagreement is recomputed here rather than
 * remembered, so a new one fails this test instead of quietly misleading
 * someone editing a property.
 */
test('the binding disagrees with the catalogue in exactly the known places', () => {
  const drifted = new Map();
  for (const prop of props) {
    if (prop.editor === 'enum') continue;
    const wire = value(prop.name, SAMPLE[prop.editor]);
    const [tag] = Object.keys(wire);
    if (prop.values.includes(tag)) continue;
    drifted.set(prop.name, CONTROL[tag] ?? prop.editor);
  }
  assert.deepEqual(Object.fromEntries(drifted), BINDING);
});

test('the controls follow the current binding', () => {
  assert.deepEqual(BINDING, {});
  assert.equal(editorOf({ name: 'Grow', editor: 'number' }), 'number');
  assert.deepEqual(value('Grow', 1), { Number: 1 });
  assert.equal(editorOf({ name: 'RowHeight', editor: 'number' }), 'number');
  assert.deepEqual(value('RowHeight', 2), { Number: 2 });
});

test('an enum property takes every member the catalogue lists', () => {
  for (const prop of props.filter((entry) => entry.editor === 'enum')) {
    for (const member of control(prop).members) {
      const wire = value(prop.name, member.value);
      const [tag] = Object.keys(wire);
      assert.ok(prop.values.includes(tag), `${prop.name} encodes as ${tag}, which it does not read`);
    }
  }
});
