import assert from 'node:assert/strict';
import test from 'node:test';
import React from 'react';
import { RESOURCE_STATE_TEXT_BYTE_LIMIT, ResourceState, Text } from '../dist/index.js';
import { Surface, reconciler } from '../dist/reconciler.js';

const h = React.createElement;
function stage() {
  const frames = [];
  const surface = new Surface((frame) => frames.push(frame));
  const root = reconciler.createContainer(surface, 0, null, false, null, '', () => {}, null);
  return { frames, surface, render: (node) => reconciler.updateContainer(node, root, null, null) };
}
function labels(frames) {
  return frames
    .flatMap((frame) => frame.patches)
    .filter((patch) => patch.SetProp?.prop === 'Label')
    .map((patch) => patch.SetProp.value.Text);
}

test('resource states are mutually exclusive and ready reveals children', () => {
  const view = stage();
  for (const [state, expected] of [
    ['loading', 'Fetching containers'],
    ['empty', 'No containers'],
    ['error', 'This view could not be completed.'],
    ['ready', 'api running'],
  ]) {
    view.render(
      h(
        ResourceState,
        {
          state,
          loadingLabel: 'Fetching containers',
          emptyLabel: 'No containers',
          error: 'Host unavailable',
        },
        h(Text, { label: 'api running' }),
      ),
    );
    const visible = labels(view.frames);
    assert(visible.includes(expected));
  }
});

test('error retry dispatches exactly once and bounds host text', () => {
  let retries = 0;
  const view = stage();
  view.render(
    h(ResourceState, {
      state: 'error',
      error: 'é'.repeat(2000),
      retryLabel: 'Retry inventory',
      onRetry: () => {
        retries += 1;
      },
    }),
  );
  const patches = view.frames.flatMap((frame) => frame.patches);
  const retry = patches.find(
    (patch) => patch.SetProp?.prop === 'Label' && patch.SetProp.value.Text === 'Retry inventory',
  ).SetProp.id;
  const parent = patches.find((patch) => patch.Insert?.child === retry)?.Insert.parent;
  assert.deepEqual(
    patches.findLast(
      (patch) => patch.SetProp?.id === retry && patch.SetProp.prop === 'Size',
    )?.SetProp.value,
    { ControlSize: 'Small' },
    'shared recovery actions use the compact control height',
  );
  assert.equal(
    patches.find((patch) => patch.Create?.id === parent)?.Create.tag,
    'Row',
    'the retry action retains its compact intrinsic width',
  );
  const error = patches.find(
    (patch) => patch.SetProp?.prop === 'Label' && patch.SetProp.value.Text.startsWith('é'),
  ).SetProp.value.Text;
  assert(new TextEncoder().encode(error).byteLength <= RESOURCE_STATE_TEXT_BYTE_LIMIT);
  assert(view.surface.dispatch({ trigger: 'Invoke', node: retry, id: `${retry}:Invoke` }));
  assert.equal(retries, 1);
});

test('frame failures lead with recovery and disclose bounded diagnostics separately', () => {
  const view = stage();
  view.render(
    h(ResourceState, {
      state: 'error',
      operation: 'Extension connection',
      error: 'expected frame 8, received frame 10',
      onRetry() {},
    }),
  );
  const patches = view.frames.flatMap((frame) => frame.patches);
  const visible = labels(view.frames);
  assert(
    visible.includes(
      'Extension connection lost sync with the extension host. No change was assumed.',
    ),
  );
  assert(visible.includes('Technical details'));
  assert(visible.includes('expected frame 8, received frame 10'));
  assert(patches.some((patch) => patch.Create?.tag === 'Expander'));
});

test('invalid state and retry contracts fail closed', () => {
  const view = stage();
  assert.throws(() => view.render(h(ResourceState, { state: 'stale' })), /state must be/);
  assert.throws(
    () => view.render(h(ResourceState, { state: 'error', onRetry: true })),
    /onRetry must be/,
  );
});
