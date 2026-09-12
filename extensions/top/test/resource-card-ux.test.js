import assert from 'node:assert/strict';
import test from 'node:test';
import { createElement as h } from 'react';
import { Containers, Executions, Images, Networks, Volumes } from '../dist/app.js';
import { host } from './host.js';

const resource = (data) => ({ data, loading: false, error: null, reload: async () => {} });

test('image inventory is a full-width compact summary with secondary inspection and disclosed destruction', async () => {
  const stage = host();
  const frame = stage.render(
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
  const card = frame.patches.find((patch) => patch.Create?.tag === 'Card')?.Create.id;
  assert.ok(card, 'the image inventory renders a resource card');
  assert.ok(
    frame.patches.some(
      (patch) =>
        patch.SetProp?.id === card &&
        patch.SetProp.prop === 'Width' &&
        patch.SetProp.value?.Length === 'Fill',
    ),
    'the resource card consumes the readable page width',
  );
  assert.equal(
    frame.patches.some((patch) => patch.Create?.tag === 'CardActions'),
    false,
    'a lone inspection action does not create a detached footer band',
  );
  const inspect = frame.patches.find(
    (patch) => patch.SetProp?.prop === 'Label' && patch.SetProp.value?.Text === 'Inspect',
  )?.SetProp.id;
  assert.ok(
    frame.patches.some(
      (patch) =>
        patch.SetProp?.id === inspect &&
        patch.SetProp.prop === 'Size' &&
        patch.SetProp.value?.ControlSize === 'Small',
    ),
    'the frequent inspect operation remains compact',
  );
  assert.ok(
    frame.patches.some(
      (patch) =>
        patch.SetProp?.id === inspect &&
        patch.SetProp.prop === 'Variant' &&
        patch.SetProp.value?.Variant === 'Outline',
    ),
    'inspection remains a secondary action rather than competing with image pull',
  );
  assert.deepEqual(ancestorTags(stage, 'Inspect').slice(0, 3), ['Row', 'Row', 'CardContent']);
  assert.deepEqual(ancestorTags(stage, 'Danger zone').slice(0, 3), ['Row', 'Row', 'CardContent']);
  assert.deepEqual(property(stage, 'Danger zone', 'Variant'), { Variant: 'Outline' });
  assert.deepEqual(property(stage, 'Danger zone', 'Width'), { Length: 'Content' });
  assert.deepEqual(property(stage, 'Danger zone', 'Tooltip'), {
    Text: 'Remove this image from the workspace image store',
  });
  assert.deepEqual(property(stage, 'Remove', 'Size'), { ControlSize: 'Small' });
  invoke(stage, 'Inspect');
  await settled();
  assert.ok(
    labelled(
      stage,
      'Verify that the image still exists and that this extension has access to it, then retry inspection.',
    ),
  );
});

test('network inventory keeps management and destructive disclosure in one compact summary row', () => {
  const stage = host();
  stage.render(
    h(Networks, {
      api: { networks: {} },
      resource: resource([
        {
          id: 'a'.repeat(32),
          name: 'development',
          driver: 'bridge',
          scope: 'local',
          kind: 'custom',
          endpoints: { containers: [], truncated: false },
        },
      ]),
      containers: resource([]),
      onOpenExtensions: () => {},
    }),
  );

  assert.deepEqual(ancestorTags(stage, 'Manage connections').slice(0, 3), [
    'Row',
    'Row',
    'CardContent',
  ]);
  assert.deepEqual(ancestorTags(stage, 'Danger zone').slice(0, 3), ['Row', 'Row', 'CardContent']);
  assert.deepEqual(property(stage, 'Danger zone', 'Tooltip'), {
    Text: 'Remove this network from the workspace',
  });
  assert.deepEqual(property(stage, 'Remove', 'Size'), { ControlSize: 'Small' });
});

test('volume authority refusal gives one recovery path and withholds removal', async () => {
  const denied = Object.assign(new Error('outside the consented resource scope'), {
    kind: 'denied',
  });
  const stage = host();
  let openedExtensions = 0;
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
      onOpenExtensions: () => {
        openedExtensions += 1;
      },
    }),
  );

  assert.ok(labelled(stage, 'Volume name'));
  invoke(stage, 'Inspect');
  await settled();
  assert.ok(
    labelled(stage, 'Volume access is denied. Review access in Extensions, then inspect again.'),
  );
  assert.equal(labelled(stage, 'Access required'), undefined);
  assert.equal(currentLabels(stage).includes('Remove'), false);
  assert.deepEqual(property(stage, 'Review access', 'Size'), { ControlSize: 'Small' });
  invoke(stage, 'Review access');
  assert.equal(openedExtensions, 1);
});

test('volume inventory keeps inspection and destructive disclosure in one compact summary row', () => {
  const stage = host();
  stage.render(
    h(Volumes, {
      api: { volumes: {} },
      resource: resource([{ name: 'workspace-cache', driver: 'local', generation: '7' }]),
      onOpenExtensions: () => {},
    }),
  );

  assert.deepEqual(property(stage, 'Inspect', 'Size'), { ControlSize: 'Small' });
  assert.deepEqual(property(stage, 'Inspect', 'Variant'), { Variant: 'Outline' });
  assert.deepEqual(ancestorTags(stage, 'Inspect').slice(0, 3), ['Row', 'Row', 'CardContent']);
  assert.deepEqual(ancestorTags(stage, 'Danger zone').slice(0, 3), ['Row', 'Row', 'CardContent']);
  assert.deepEqual(property(stage, 'Danger zone', 'Tooltip'), {
    Text: 'Remove this volume and permanently delete its stored data',
  });
  assert.deepEqual(property(stage, 'Remove', 'Size'), { ControlSize: 'Small' });
});

test('container authority refusal explains recovery and withholds detail operations', async () => {
  const denied = Object.assign(new Error('outside the consented resource scope'), {
    kind: 'denied',
  });
  const stage = host();
  let openedExtensions = 0;
  stage.render(
    h(Containers, {
      api: {
        containers: {
          inspect: async () => {
            throw denied;
          },
        },
      },
      resource: resource([
        {
          id: 'container-private',
          name: 'private-api',
          image: 'internal/api:latest',
          state: 'running',
          generation: 4,
        },
      ]),
      onOpenExtensions: () => {
        openedExtensions += 1;
      },
    }),
  );

  invoke(stage, 'Details');
  await settled();
  await settled();
  assert.equal(currentLabels(stage).includes('Access required'), false);
  assert.ok(
    labelled(
      stage,
      'Top does not have permission to inspect this container. Review its exact container access in Extensions, then inspect again.',
    ),
  );
  assert.equal(
    currentLabels(stage).filter((label) => label.includes('permission to inspect this container'))
      .length,
    1,
    'the refusal is explained once',
  );
  assert.deepEqual(property(stage, 'Review access', 'Size'), { ControlSize: 'Small' });
  invoke(stage, 'Review access');
  assert.equal(openedExtensions, 1, 'the recovery action invokes application navigation');
  assert.equal(currentLabels(stage).includes('Retry details'), false);
  assert.equal(currentLabels(stage).includes('Load logs'), false);
  assert.equal(currentLabels(stage).includes('Kill'), false);
  assert.equal(currentLabels(stage).includes('Execute'), false);
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
  assert.deepEqual(property(stage, 'More actions', 'Variant'), { Variant: 'Outline' });
  assert.deepEqual(property(stage, 'More actions', 'Width'), { Length: 'Content' });
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

function ancestorTags(stage, label) {
  const patches = stage.frames.flatMap((frame) => frame.patches);
  const tags = new Map(
    patches.filter((patch) => patch.Create).map((patch) => [patch.Create.id, patch.Create.tag]),
  );
  const parents = new Map(
    patches
      .filter((patch) => patch.Insert)
      .map((patch) => [patch.Insert.child, patch.Insert.parent]),
  );
  let node = patches
    .filter((patch) => patch.SetProp?.prop === 'Label' && patch.SetProp.value?.Text === label)
    .at(-1)?.SetProp.id;
  const ancestors = [];
  while (parents.has(node)) {
    node = parents.get(node);
    ancestors.push(tags.get(node));
  }
  return ancestors;
}

function labelled(stage, label) {
  return stage.frames
    .flatMap((frame) => frame.patches)
    .filter((patch) => patch.SetProp?.prop === 'Label' && patch.SetProp.value?.Text === label)
    .at(-1);
}

function property(stage, label, prop) {
  const patches = stage.frames.flatMap((frame) => frame.patches);
  const id = patches
    .filter((patch) => patch.SetProp?.prop === 'Label' && patch.SetProp.value?.Text === label)
    .at(-1)?.SetProp.id;
  return patches.filter((patch) => patch.SetProp?.id === id && patch.SetProp.prop === prop).at(-1)
    ?.SetProp.value;
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
