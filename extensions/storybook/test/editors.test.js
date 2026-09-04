import assert from 'node:assert/strict';
import test from 'node:test';

import { component, editable, props, tags } from '../dist/catalogue.js';
import { amountOf, control, lengthValue, memberOf, modeOf, rows } from '../dist/editors.js';
import { value } from './host.js';

const byName = new Map(props.map((prop) => [prop.name, prop]));

test('every declared property gets a control, and no other property does', () => {
  for (const tag of tags) {
    const all = rows(tag.name);
    assert.deepEqual(
      all.map((row) => row.prop).sort(),
      component(tag.name).props.slice().sort(),
    );
    for (const row of all) {
      assert.ok(row.name && row.editor && row.group);
      if (row.editor === 'enum') assert.ok(row.members.length > 0, `${row.name} has an empty Select`);
      if (row.editor === 'length' || row.editor === 'edges') assert.ok(row.maximum > 0);
    }
  }
});

test('components with different contracts get different property rows', () => {
  assert.notDeepEqual(editable('Button'), editable('Separator'));
  assert.ok(rows('Button').some((row) => row.prop === 'Label'));
  assert.ok(!rows('Separator').some((row) => row.prop === 'Label'));
});

test('the editable properties come first', () => {
  const editable = rows('Button').map((row) => row.editable);
  assert.deepEqual(editable, editable.slice().sort((left, right) => Number(right) - Number(left)));
});

test('a Select is populated from the catalogue, in the spelling JSX uses', () => {
  const tone = control(byName.get('Tone'));
  assert.deepEqual(
    tone.members.map((member) => member.value),
    ['neutral', 'accent', 'positive', 'warning', 'danger'],
  );
  const color = control(byName.get('Color'));
  assert.ok(color.members.some((member) => member.value === 'text-dim'));
  assert.equal(memberOf(byName.get('Color'), 'TextDim'), 'text-dim');
});

test('editing a property produces the wire value the host expects', () => {
  assert.deepEqual(value('Tone', 'accent'), { Tone: 'Accent' });
  assert.deepEqual(value('Gap', lengthValue('step', 2)), { Length: { Step: 2 } });
  assert.deepEqual(value('Width', lengthValue('chars', 12)), { Length: { Chars: 12 } });
  assert.deepEqual(value('Width', lengthValue('fill', 0)), { Length: 'Fill' });
  assert.deepEqual(value('Label', 'Go'), { Text: 'Go' });
  assert.deepEqual(value('Enabled', true), { Flag: true });
  assert.deepEqual(value('Columns', 3), { Integer: 3 });
  assert.deepEqual(value('Fraction', 0.5), { Number: 0.5 });
  assert.deepEqual(value('Pad', lengthValue('step', 4)), { Length: { Step: 4 } });
});

test('a length control shows the mode the value is already in', () => {
  assert.equal(modeOf(2), 'step');
  assert.equal(amountOf(2), 2);
  assert.equal(modeOf({ chars: 12 }), 'chars');
  assert.equal(amountOf({ chars: 12 }), 12);
  assert.equal(modeOf('fill'), 'fill');
  assert.equal(modeOf('content'), 'content');
});

test('every control produces a value its property accepts', () => {
  for (const row of props.map(control).filter((entry) => entry.editable)) {
    const prop = byName.get(row.prop);
    const sample = {
      text: 'text',
      switch: true,
      enum: row.members?.[0]?.value,
      number: 1,
      length: lengthValue('step', 1),
      edges: lengthValue('step', 1),
    }[row.editor];
    const wire = value(prop.name, sample);
    assert.equal(typeof wire, 'object', `${row.name} produced no tagged value`);
    assert.equal(Object.keys(wire).length, 1);
  }
});
