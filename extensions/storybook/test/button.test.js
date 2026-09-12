import assert from 'node:assert/strict';
import test from 'node:test';
import { createElement as h } from 'react';

import { ButtonWorkbench } from '../dist/button.js';
import { host } from './host.js';
import { readFile } from 'node:fs/promises';

function propsFor(patches, tag) {
  const ids = patches.filter((patch) => patch.Create?.tag === tag).map((patch) => patch.Create.id);
  return ids.map((id) =>
    Object.fromEntries(
      patches
        .filter((patch) => patch.SetProp?.id === id)
        .map((patch) => [patch.SetProp.prop, patch.SetProp.value]),
    ),
  );
}

test('Button documents every semantic size, variant, and tone as live controls', () => {
  const frame = host().render(h(ButtonWorkbench));
  const buttons = propsFor(frame.patches, 'Button');

  for (const size of ['Small', 'Medium', 'Large']) {
    assert(
      buttons.some((props) => props.Size?.ControlSize === size),
      `missing ${size} size`,
    );
  }
  for (const variant of ['Filled', 'Outline', 'Ghost', 'Plain']) {
    assert(
      buttons.some((props) => props.Variant?.Variant === variant),
      `missing ${variant}`,
    );
  }
  for (const tone of ['Neutral', 'Accent', 'Positive', 'Warning', 'Danger']) {
    assert(
      buttons.some((props) => props.Tone?.Tone === tone),
      `missing ${tone}`,
    );
  }
  assert.equal(
    propsFor(frame.patches, 'IconButton').length,
    0,
    'IconButton belongs on its own page',
  );
});

test('Button teaches specimens before its full API and keeps the playground secondary', () => {
  const frame = host().render(h(ButtonWorkbench));
  const headings = propsFor(frame.patches, 'Heading').map((props) => props.Label?.Text);
  const expanders = propsFor(frame.patches, 'Expander').map((props) => props.Label?.Text);

  assert.deepEqual(headings.slice(0, 4), ['Button', 'Overview', 'Basic', 'Variants']);
  assert.ok(headings.indexOf('States') < headings.indexOf('API'));
  assert(expanders.includes('Playground'));
});

test('shared specimen pairs preserve source order in a narrow single column', async () => {
  const source = await readFile(new URL('../src/component-document.tsx', import.meta.url), 'utf8');
  const specimen = source.slice(
    source.indexOf('export function SpecimenGrid'),
    source.indexOf('export function ApiReference'),
  );
  assert.match(specimen, /<Column gap=\{3\} width="fill">/);
  assert.doesNotMatch(specimen, /columns=\{2\}/);
});

test('Button keeps its overview code and every size row bounded to the document width', async () => {
  const source = await readFile(new URL('../src/button.tsx', import.meta.url), 'utf8');
  assert.match(source, /<Code\s+width="fill"[\s\S]*?wrap/);
  assert.match(source, /<Row gap=\{2\} wrap width="fill">/);
  assert.match(source, /label=\{`\$\{title\(controlSize\)\} · \$\{height\(controlSize\)\}px`\}/);
  assert.match(source, /label=\{title\(emphasis\)\}/);
});

test('Button documents a compact reset beside the value it affects', () => {
  const frame = host().render(h(ButtonWorkbench));
  const buttons = propsFor(frame.patches, 'Button');
  assert(
    buttons.some(
      (props) =>
        props.Label?.Text === 'Clear product access' &&
        props.Size?.ControlSize === 'Small' &&
        props.Variant?.Variant === 'Ghost',
    ),
  );
});

test('Button documents a labelled non-invokable busy state', () => {
  const frame = host().render(h(ButtonWorkbench));
  const buttons = propsFor(frame.patches, 'Button');
  assert(buttons.some((props) => props.Label?.Text === 'Saving…' && props.Busy?.Flag === true));
});
