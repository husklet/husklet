import assert from 'node:assert/strict';
import test from 'node:test';
import { createElement as h } from 'react';
import { SliderWorkbench, boundedStep } from '../dist/slider.js';
import { host } from './host.js';

function labels(patches) {
  return patches
    .filter((patch) => patch.SetProp?.prop === 'Label')
    .map((patch) => patch.SetProp.value?.Text);
}

test('Slider clamps and advances in exact declared steps', () => {
  assert.equal(boundedStep(-7), 0);
  assert.equal(boundedStep(43), 45);
  assert.equal(boundedStep(104), 100);
  const stage = host();
  const first = stage.render(h(SliderWorkbench));
  const slider = first.patches.find((patch) => patch.Create?.tag === 'Slider')?.Create.id;
  assert.ok(slider);
  const before = stage.frames.length;
  assert(
    stage.surface.dispatch({ trigger: 'Change', node: slider, id: `${slider}:Change`, value: 45 }),
  );
  assert(labels(stage.since(before)).includes('Current value: 45%'));
});

test('Slider documents bounds, steps, disabled silence, and keyboard behavior before API', () => {
  const stage = host();
  const frame = stage.render(h(SliderWorkbench));
  const text = labels(frame.patches);
  for (const label of [
    'Minimum · 0',
    'Middle · 50',
    'Maximum · 100',
    'Disabled · 35',
    'Coarse · step 25',
    'Fine · step 0.1',
  ])
    assert(text.includes(label));
  assert(text.some((label) => label?.includes('Home selects minimum')));
  assert(text.some((label) => label?.includes('neither move nor report changes')));
  assert(text.indexOf('Range states') < text.indexOf('API'));
  const sliders = frame.patches
    .filter((patch) => patch.Create?.tag === 'Slider')
    .map((patch) => patch.Create.id);
  const disabled = sliders[4];
  assert.equal(
    stage.surface.dispatch({
      trigger: 'Change',
      node: disabled,
      id: `${disabled}:Change`,
      value: 40,
    }),
    false,
  );
});
