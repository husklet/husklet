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
  const changed = stage.since(before);
  assert(labels(changed).includes('Build cache · 45%'));
  assert(labels(changed).includes('Current value: 45%'));
  assert(
    changed.some(
      (patch) =>
        patch.SetProp?.id === slider &&
        patch.SetProp.prop === 'Value' &&
        patch.SetProp.value?.Integer === 45,
    ),
  );
});

test('Slider documents bounds, steps, disabled silence, and keyboard behavior before API', () => {
  const stage = host();
  const frame = stage.render(h(SliderWorkbench));
  const text = labels(frame.patches);
  for (const label of [
    'Range anatomy · minimum 0 · midpoint 50 · maximum 100',
    'Disabled · 35',
    'Coarse · step 25',
    'Fine · step 0.1',
  ])
    assert(text.includes(label));
  assert(text.some((label) => label?.includes('Home selects minimum')));
  assert(text.some((label) => label?.includes('neither move nor report changes')));
  assert(text.indexOf('Range states') < text.indexOf('API'));
  const source = frame.patches.find(
    (patch) => patch.SetProp?.prop === 'Value' && patch.SetProp.value?.Text?.includes('<Slider'),
  )?.SetProp.value.Text;
  assert(source.includes('onChange={(report) =>'));
  assert(source.includes('setValue(boundedStep(report.value))'));
  const sliders = frame.patches
    .filter((patch) => patch.Create?.tag === 'Slider')
    .map((patch) => patch.Create.id);
  const disabled = sliders[2];
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

test('Slider code disclosure starts collapsed and reveals the exact report handler', () => {
  const stage = host();
  const frame = stage.render(h(SliderWorkbench));
  const label = frame.patches.find(
    (patch) => patch.SetProp?.prop === 'Label' && patch.SetProp.value?.Text === 'Show code',
  )?.SetProp;
  assert.ok(label);
  assert(
    frame.patches.some(
      (patch) =>
        patch.SetProp?.id === label.id &&
        patch.SetProp.prop === 'Expanded' &&
        patch.SetProp.value?.Flag === false,
    ),
  );
  const before = stage.frames.length;
  assert(
    stage.surface.dispatch({
      trigger: 'Expand',
      node: label.id,
      id: `${label.id}:Expand`,
      value: true,
    }),
  );
  const changed = stage.since(before);
  assert(
    changed.some(
      (patch) =>
        patch.SetProp?.id === label.id &&
        patch.SetProp.prop === 'Expanded' &&
        patch.SetProp.value?.Flag === true,
    ),
  );
  const source = frame.patches.find(
    (patch) => patch.SetProp?.prop === 'Value' && patch.SetProp.value?.Text?.includes('<Slider'),
  )?.SetProp.value.Text;
  assert(source.includes('setValue(boundedStep(report.value))'));
});
