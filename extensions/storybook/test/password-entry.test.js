import assert from 'node:assert/strict';
import test from 'node:test';
import { createElement as h } from 'react';
import { componentPages } from '../dist/component-pages.js';
import { PasswordEntryWorkbench } from '../dist/password-entry.js';
import { host } from './host.js';

const creations = (frame, tag) =>
  frame.patches.filter((patch) => patch.Create?.tag === tag).map((patch) => patch.Create.id);
const properties = (frame, id) =>
  Object.fromEntries(
    frame.patches
      .filter((patch) => patch.SetProp?.id === id)
      .map((patch) => [patch.SetProp.prop, patch.SetProp.value]),
  );

test('PasswordEntry documents reveal policy and genuine states', () => {
  assert.equal(componentPages.PasswordEntry, PasswordEntryWorkbench);
  const frame = host().render(h(PasswordEntryWorkbench));
  const headings = creations(frame, 'Heading').map((id) => properties(frame, id).Label?.Text);
  assert.deepEqual(headings.slice(0, 7), [
    'PasswordEntry',
    'Overview',
    'Reveal policy',
    'States',
    'Behavior',
    'Accessibility',
    'API',
  ]);
  const fields = creations(frame, 'PasswordEntry').map((id) => properties(frame, id));
  assert.equal(fields.length, 5);
  assert(fields.some((props) => props.Secret?.Flag === true));
  assert(fields.some((props) => props.Secret?.Flag === false));
  assert(fields.some((props) => props.Enabled?.Flag === false));
});

test('PasswordEntry retains its controlled value without echoing it in feedback', () => {
  const stage = host();
  const frame = stage.render(h(PasswordEntryWorkbench));
  const field = creations(frame, 'PasswordEntry')[0];
  const before = stage.frames.length;
  assert(
    stage.surface.dispatch({
      trigger: 'Change',
      node: field,
      id: `${field}:Change`,
      value: 'updated-secret',
    }),
  );
  const patches = stage.since(before);
  assert(
    patches.some(
      (patch) =>
        patch.SetProp?.id === field &&
        patch.SetProp.prop === 'Value' &&
        patch.SetProp.value.Text === 'updated-secret',
    ),
  );
  assert(patches.some((patch) => patch.SetProp?.value?.Text === '14 characters · concealed'));
  assert(
    !patches.some(
      (patch) => patch.SetProp?.value?.Text === 'updated-secret' && patch.SetProp?.id !== field,
    ),
  );
});
