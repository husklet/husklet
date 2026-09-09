import assert from 'node:assert/strict';
import test from 'node:test';
import { createElement as h } from 'react';
import { Executions, Images, Volumes } from '../dist/app.js';
import { host } from './host.js';

const resource = (data) => ({ data, loading: false, error: null, reload: async () => {} });

test('image inventory labels pull input and keeps destructive actions disclosed', async () => {
  const stage = host();
  stage.render(
    h(Images, {
      api: {
        images: {
          inspect: async () => {
            throw new Error('manifest disappeared');
          },
        },
      },
      resource: resource([{ id: 'sha256:image', reference: 'alpine:3.20', size: 1024 }]),
    }),
  );

  assert.ok(labelled(stage, 'Image reference'));
  assert.ok(labelled(stage, 'Image maintenance'));
  assert.ok(labelled(stage, 'Danger zone'));
  invoke(stage, 'Inspect');
  await settled();
  assert.ok(
    labelled(
      stage,
      'Verify that the image still exists and that this extension has access to it, then retry inspection.',
    ),
  );
});

test('volume authority refusal gives one recovery path and withholds removal', async () => {
  const denied = Object.assign(new Error('outside the consented resource scope'), {
    kind: 'denied',
  });
  const stage = host();
  stage.render(
    h(Volumes, {
      api: {
        volumes: {
          inspect: async () => {
            throw denied;
          },
        },
      },
      resource: resource([{ name: 'private-data', driver: 'local', generation: '7' }]),
    }),
  );

  assert.ok(labelled(stage, 'Volume name'));
  invoke(stage, 'Inspect');
  await settled();
  assert.ok(labelled(stage, 'Access required'));
  assert.ok(
    labelled(
      stage,
      'This extension was not granted access to inspect this volume. Change its exact volume access from Extensions, then inspect again.',
    ),
  );
  assert.equal(currentLabels(stage).includes('Remove'), false);
});

test('execution output has an observable loading state and explicit empty result', async () => {
  let finish;
  const logs = new Promise((resolve) => {
    finish = resolve;
  });
  const item = {
    id: 'e'.repeat(32),
    container_id: 'c'.repeat(32),
    running: false,
    exit_code: 0,
    pid: 0,
    command: ['true'],
    user: '',
  };
  const stage = host();
  stage.render(
    h(Executions, {
      api: { containers: { execution: async () => item, executionLogs: async () => logs } },
      resource: resource([item]),
      requestedExecution: item.id,
    }),
  );
  await settled();
  invoke(stage, 'Load output');
  await settled();
  assert.ok(labelled(stage, 'Loading captured output…'));
  finish({ stdout: '', stderr: '', truncated: false, eof: true });
  await until(() => textProperty(stage, 'No stdout captured (EOF).'));
  assert.ok(textProperty(stage, 'No stdout captured (EOF).'));
  assert.ok(textProperty(stage, 'No stderr captured (EOF).'));
  assert.ok(labelled(stage, 'More actions'));
});

function currentLabels(stage) {
  const labels = new Map();
  const parents = new Map();
  const removed = new Set();
  for (const frame of stage.frames) {
    for (const patch of frame.patches) {
      if (patch.Insert) {
        parents.set(patch.Insert.child, patch.Insert.parent);
        removed.delete(patch.Insert.child);
      }
      if (patch.Remove) removed.add(patch.Remove.id);
      if (patch.SetProp?.prop === 'Label') labels.set(patch.SetProp.id, patch.SetProp.value?.Text);
    }
  }
  const active = (node) => {
    for (let current = node; current !== undefined; current = parents.get(current)) {
      if (removed.has(current)) return false;
    }
    return true;
  };
  return [...labels].filter(([node]) => active(node)).map(([, label]) => label);
}

function labelled(stage, label) {
  return stage.frames
    .flatMap((frame) => frame.patches)
    .filter((patch) => patch.SetProp?.prop === 'Label' && patch.SetProp.value?.Text === label)
    .at(-1);
}

function textProperty(stage, value) {
  return stage.frames
    .flatMap((frame) => frame.patches)
    .some((patch) =>
      Object.values(patch.SetProp?.value ?? {}).some((property) => property === value),
    );
}

function invoke(stage, label) {
  const nodes = stage.frames
    .flatMap((frame) => frame.patches)
    .filter((patch) => patch.SetProp?.prop === 'Label' && patch.SetProp.value?.Text === label)
    .map((patch) => patch.SetProp.id)
    .reverse();
  assert.ok(
    nodes.some((node) =>
      stage.surface.dispatch({ trigger: 'Invoke', node, id: `${node}:Invoke`, value: null }),
    ),
  );
}

const settled = () => new Promise((resolve) => setImmediate(resolve));
async function until(predicate) {
  for (let attempt = 0; attempt < 50; attempt += 1) {
    if (predicate()) return;
    await settled();
  }
  assert.fail('condition did not become true');
}
