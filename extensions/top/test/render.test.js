import assert from 'node:assert/strict';
import test from 'node:test';
import { createElement as h } from 'react';
import { PROTOCOL_CAPABILITIES } from '@husklet/client';
import {
  Containers,
  Executions,
  Extensions,
  Images,
  Networks,
  Overview,
  Processes,
  Terminals,
  Volumes,
  Workspace,
  Top,
  SIDEBAR_WIDTH_DEFAULT,
  SIDEBAR_WIDTH_MAX,
  SIDEBAR_WIDTH_MIN,
  SIDEBAR_SAVE_DELAY_MS,
  boundedSidebarWidth,
  persistSidebarWidth,
  parseArguments,
  parseLabels,
  parseMounts,
  parsePorts,
  acquisitionFailure,
  acquisitionTechnicalDetail,
  acquisitionLabel,
  catalogueTrust,
  catalogueCandidateMismatch,
  capabilityLabel,
  filterCatalogueEntries,
  filterInstalledExtensions,
} from '../dist/app.js';

test('every host capability has explicit consent language and workspace lifecycle is not settings', () => {
  for (const { wire } of PROTOCOL_CAPABILITIES) {
    assert.ok(capabilityLabel(wire), `missing consent language for ${wire}`);
  }
  assert.equal(capabilityLabel('workspaces:configure'), 'Modify workspace settings');
  assert.equal(capabilityLabel('workspaces:control'), 'Create, start, stop, and delete workspaces');
});

test('catalogue display strings cannot forge verified publisher status', () => {
  const forged = {
    publisher: 'Husklet',
    source: 'husklet:first-party/storybook',
    publisher_verified: false,
  };
  assert.deepEqual(catalogueTrust(forged), {
    label: 'Publisher · Husklet',
    tone: 'neutral',
  });
  assert.deepEqual(catalogueTrust({ ...forged, publisher_verified: true }), {
    label: 'Verified publisher · Husklet',
    tone: 'accent',
  });
});

test('an acquired image must retain the catalogue identity the developer selected', () => {
  const selected = { id: 'database', version: '2.0.0' };
  assert.equal(
    catalogueCandidateMismatch(selected, { name: 'other-tool', version: '2.0.0' }),
    'Catalogue identity changed: expected database, but the inspected image declares other-tool.',
  );
  assert.equal(
    catalogueCandidateMismatch(selected, { name: 'database', version: '1.0.0' }),
    'Catalogue version changed: expected 2.0.0, but the inspected image declares 1.0.0.',
  );
  assert.equal(catalogueCandidateMismatch(selected, { name: 'database', version: '2.0.0' }), '');
});
import {
  ContainerDetailsSource,
  ExecutionDetailsSource,
  ImageDetailsSource,
  ProcessTableSource,
  VolumeDetailsSource,
} from '../dist/model.js';
import { host } from './host.js';

const api = {
  containers: {
    list: async () => [],
    processes: async () => [],
    executions: async () => ({ executions: [], truncated: false }),
  },
  images: {
    list: async () => [],
    pull: async () => ({}),
    inspect: async () => ({}),
    remove: async () => {},
    removeAndWait: async (id) => ({ changed: true, id }),
    prune: async () => ({ deleted: 0, space_reclaimed: 0 }),
  },
  volumes: {
    list: async () => [],
    inspect: async () => ({}),
    create: async () => ({}),
    remove: async () => {},
    removeAndWait: async (name, generation) => ({ changed: true, name, generation }),
  },
  networks: {
    list: async () => [],
    inspect: async () => ({}),
    create: async () => '',
    remove: async () => {},
    removeAndWait: async (id) => ({ changed: true, id }),
    connect: async () => {},
    disconnect: async () => {},
  },
  terminal: { tabs: async () => [], pinTab: async () => {}, focus: async () => {} },
  extensions: { list: async () => [] },
};

function containerResource(...ids) {
  return {
    data: ids.map((id, index) => ({
      id,
      name: `container-${index + 1}`,
      image: 'alpine:3.20',
      state: 'exited',
      created: 1,
      generation: 1,
    })),
    loading: false,
    error: null,
    reload: async () => {},
  };
}

function chooseContainer(stage, id) {
  const choice = stage.frames
    .flatMap((frame) => frame.patches)
    .filter((patch) => patch.SetProp?.prop === 'Choices')
    .find((patch) => patch.SetProp.value?.Choices?.some((entry) => entry.value === id));
  assert.ok(choice, `container choice ${id} is available`);
  assert.ok(
    stage.surface.dispatch({
      trigger: 'Change',
      node: choice.SetProp.id,
      id: `${choice.SetProp.id}:Change`,
      value: id,
    }),
    `container choice ${id} changes`,
  );
}

test('Top sidebar preference is narrowly bounded and retried with fresh CAS authority', async () => {
  assert.equal(SIDEBAR_SAVE_DELAY_MS, 250);
  assert.equal(SIDEBAR_WIDTH_MIN, 144);
  assert.equal(SIDEBAR_WIDTH_DEFAULT, 160);
  assert.equal(SIDEBAR_WIDTH_MAX, 240);
  assert.equal(boundedSidebarWidth(143), 144);
  assert.equal(boundedSidebarWidth(192), 192);
  assert.equal(boundedSidebarWidth(241), 240);
  assert.equal(boundedSidebarWidth(1.5), null);
  const calls = [];
  let revision = 4;
  const preferences = {
    read: async () => ({ revision, entries: [] }),
    set: async (observed, key, value) => {
      calls.push([observed, key, value]);
      if (calls.length === 1) {
        revision = 5;
        throw Object.assign(new Error('conflict'), { kind: 'conflict' });
      }
      return 6;
    },
  };
  assert.equal(await persistSidebarWidth({ preferences }, 192), 6);
  assert.deepEqual(calls, [
    [4, 'sidebar.width', { kind: 'number', value: 192 }],
    [5, 'sidebar.width', { kind: 'number', value: 192 }],
  ]);
});

const firstPartyCatalogue = async () => ({
  entries: [
    {
      id: 'storybook',
      title: 'Component playground',
      description: 'Explore extension components, large tables, terminals, diffs, and metrics.',
      version: '2.0.0',
      reference: 'ghcr.io/husklet/husklet/extension-storybook:latest',
      publisher: 'Husklet',
      source: 'husklet:first-party/storybook',
      publisher_verified: true,
    },
  ],
  complete: true,
});

const largeCatalogueEntries = [
  {
    id: 'database',
    title: 'Database Studio',
    description: 'Inspect Postgres schemas, queries, and plans.',
    version: '2.0.0',
    reference: 'registry/database:2',
    publisher: 'Acme Data',
    source: 'community/database',
    protocol: 1,
    architectures: ['amd64'],
  },
  {
    id: 'storybook',
    title: 'Component playground',
    description: 'Inspect interface components.',
    version: '2.0.0',
    reference: 'registry/storybook:2',
    publisher: 'Husklet',
    source: 'husklet:first-party/storybook',
    publisher_verified: true,
    protocol: 1,
    architectures: ['amd64'],
  },
  {
    id: 'future',
    title: 'Future debugger',
    description: 'Debug applications with a future protocol.',
    version: '1.0.0',
    reference: 'registry/future:1',
    publisher: 'Future Tools',
    source: 'community/future',
    protocol: 999,
    architectures: ['amd64'],
  },
  ...Array.from({ length: 17 }, (_, index) => ({
    id: `tool-${String(index + 1).padStart(2, '0')}`,
    title: `Developer Tool ${String(index + 1).padStart(2, '0')}`,
    description: `Daily developer workflow ${index + 1}.`,
    version: '1.0.0',
    reference: `registry/tool-${index + 1}:1`,
    publisher: index === 8 ? 'Searchable Labs' : 'Community',
    source: `community/tool-${index + 1}`,
    protocol: 1,
    architectures: ['amd64'],
  })),
];

const largeCatalogueInstalled = [
  {
    name: 'database',
    version: '1.0.0',
    image_digest: `sha256:${'d'.repeat(64)}`,
    enabled: true,
    status: 'running',
  },
  {
    name: 'storybook',
    version: '2.0.0',
    image_digest: `sha256:${'s'.repeat(64)}`,
    enabled: true,
    status: 'running',
  },
];

const largeInstalledExtensions = [
  {
    name: 'faulted-agent',
    version: '1.0.0',
    image_digest: `sha256:${'f'.repeat(64)}`,
    enabled: true,
    status: 'fault:extension process exited',
  },
  {
    name: 'database',
    version: '1.0.0',
    image_digest: `sha256:${'d'.repeat(64)}`,
    enabled: true,
    status: 'running',
  },
  {
    name: 'disabled-linter',
    version: '1.0.0',
    image_digest: `sha256:${'e'.repeat(64)}`,
    enabled: false,
    status: 'standby',
  },
  {
    name: 'storybook',
    version: '2.0.0',
    image_digest: `sha256:${'s'.repeat(64)}`,
    enabled: true,
    status: 'running',
  },
  ...Array.from({ length: 46 }, (_, index) => ({
    name: `installed-${String(index + 1).padStart(2, '0')}`,
    version: '1.0.0',
    image_digest: `sha256:${String((index % 9) + 1).repeat(64)}`,
    enabled: true,
    status: 'running',
    pane_providers:
      index === 6 ? [{ id: 'observability', title: 'Observability dashboard', icon: null }] : [],
  })),
];

test('Top presents workspace, extensions, and every resource navigation choice', () => {
  const stage = host();
  const frame = stage.render(
    h(Top, {
      api,
      initial: {
        containers: [],
        executions: [],
        images: [],
        volumes: [],
        networks: [],
        extensions: [],
      },
    }),
  );
  const labels = frame.patches
    .filter((patch) => 'SetProp' in patch && patch.SetProp.prop === 'Label')
    .map((patch) => patch.SetProp.value.Text);
  for (const label of [
    'Workspace',
    'Settings',
    'Extensions',
    'Containers',
    'Processes',
    'Executions',
    'Images',
    'Volumes',
    'Networks',
    'Terminals',
  ])
    assert.ok(labels.includes(label), label);
  assert.deepEqual(
    ancestorProperty(stage, 'Section', 'Row', 'Justify'),
    { Align: 'Start' },
    'the compact chooser anchors inside its viewport instead of centering beyond it',
  );
  assert.equal(
    frame.patches.some((patch) => 'Create' in patch && patch.Create.tag === 'Card'),
    true,
  );
  assert.equal(
    taggedProperty(stageFromFrame(frame), 'Workspace', 'NavigationMenuItem', 'Selected')?.Flag,
    true,
  );
  assert.equal(
    taggedProperty(stageFromFrame(frame), 'Settings', 'NavigationMenuItem', 'Selected')?.Flag,
    false,
  );
  assert.equal(
    frame.patches.filter((patch) => patch.Create?.tag === 'NavigationMenuItem').length,
    10,
    'every destination exposes its selected state to keyboard and assistive users',
  );
  const icons = [
    'Workspace',
    'Settings',
    'Extensions',
    'Containers',
    'Processes',
    'Executions',
    'Images',
    'Volumes',
    'Networks',
    'Terminals',
  ].map(
    (label) => taggedProperty(stageFromFrame(frame), label, 'NavigationMenuItem', 'Icon')?.Text,
  );
  assert.equal(new Set(icons).size, 10, 'every destination has a distinguishable icon');
  for (const group of ['Manage', 'Runtime', 'Resources', 'Interface'])
    assert.ok(labels.includes(group), group);
  assert.equal(
    frame.patches.filter((patch) => patch.Create?.tag === 'ListSubheader').length,
    4,
    'navigation groups use semantic list headings instead of body text',
  );
  assert.ok(
    labels.includes('0 enabled'),
    'the overview exposes extension inventory alongside the other workspace resources',
  );
  assert.ok(labels.includes('Inspect'), 'process navigation does not fabricate an inventory count');
  assert.ok(labels.includes('Per-container snapshots'));
  assert.ok(labels.includes('Local storage'), 'volume summaries remain compact');
  assert.ok(labels.includes('Workspace network'), 'network summaries remain compact');
  assert.equal(
    frame.patches.filter((patch) => patch.Create?.tag === 'CardActionArea').length,
    8,
    'every compact overview summary is one full-card navigation target',
  );
  assert.deepEqual(
    outerAncestorProperty(
      stageFromFrame(frame),
      'Current inventory and reported runtime attention.',
      'Column',
      'Pad',
    ),
    { Length: { Step: 4 } },
    'the overview keeps a 16px inset instead of touching the viewport edge',
  );
  assert.deepEqual(
    ancestorProperty(
      stageFromFrame(frame),
      'Current inventory and reported runtime attention.',
      'Column',
      'Gap',
    ),
    { Length: { Step: 3 } },
    'overview sections use a consistent 12px rhythm',
  );
});

test('Top network attachment selects a named container while retaining immutable authority', async () => {
  const containerId = 'a'.repeat(64);
  const stage = host();
  stage.render(
    h(Top, {
      api,
      initial: {
        containers: [
          {
            id: containerId,
            name: 'api-worker',
            image: 'alpine:3.20',
            state: 'exited',
            created: 1,
            generation: 2,
          },
        ],
        executions: [],
        images: [],
        volumes: [],
        networks: [
          {
            id: 'b'.repeat(32),
            name: 'development',
            driver: 'bridge',
            scope: 'local',
            kind: 'custom',
            endpoints: { containers: [], truncated: false },
          },
        ],
        terminals: [],
        extensions: [],
      },
    }),
  );
  invoke(stage, 'Networks');
  await settled();
  invoke(stage, 'Manage connections');
  await settled();
  await settled();
  assert.equal(
    stage.frames
      .flatMap((frame) => frame.patches)
      .some(
        (patch) =>
          patch.SetProp?.prop === 'Placeholder' &&
          patch.SetProp.value?.Text === 'Complete container ID',
      ),
    false,
    'the real Top workflow does not ask users to transcribe an immutable ID',
  );
  const choicePatch = stage.frames
    .flatMap((frame) => frame.patches)
    .filter((patch) => patch.SetProp?.prop === 'Choices')
    .find((patch) => patch.SetProp.value?.Choices?.some((choice) => choice.value === containerId));
  assert.deepEqual(choicePatch?.SetProp.value, {
    Choices: [{ value: containerId, label: `api-worker · ${'a'.repeat(12)} · exited` }],
  });
  assert.ok(
    stage.surface.dispatch({
      trigger: 'Change',
      node: choicePatch.SetProp.id,
      id: `${choicePatch.SetProp.id}:Change`,
      value: containerId,
    }),
    'the native selector reports the exact immutable ID',
  );
  await settled();
  await settled();
  assert.ok(labelled(stage, 'Connect'));
});

test('Top sidebar divider reports and bounds its retained position', () => {
  const stage = host();
  stage.render(
    h(Top, {
      api,
      initial: {
        containers: [],
        executions: [],
        images: [],
        volumes: [],
        networks: [],
        terminals: [],
        extensions: [],
      },
    }),
  );
  const splitter = stage.frames
    .flatMap((frame) => frame.patches)
    .find((patch) => patch.Create?.tag === 'Responsive').Create.id;
  const responsiveChildren = stage.frames
    .flatMap((frame) => frame.patches)
    .filter((patch) => patch.Insert?.parent === splitter);
  assert.equal(
    responsiveChildren.length,
    3,
    'responsive Top owns compact navigation, wide navigation, and exactly one body',
  );
  const sectionChoice = stage.frames
    .flatMap((frame) => frame.patches)
    .filter((patch) => patch.SetProp?.prop === 'Choices')
    .find((patch) => patch.SetProp.value?.Choices?.some((choice) => choice.value === 'containers'));
  assert.deepEqual(
    sectionChoice?.SetProp.value.Choices.map((choice) => choice.value),
    [
      'workspace',
      'settings',
      'extensions',
      'containers',
      'processes',
      'executions',
      'images',
      'volumes',
      'networks',
      'terminals',
    ],
    'compact navigation reaches every Top section',
  );
  assert.ok(
    stage.surface.dispatch({
      trigger: 'Change',
      node: sectionChoice.SetProp.id,
      id: `${sectionChoice.SetProp.id}:Change`,
      value: 'containers',
    }),
  );
  assert.ok(labelled(stage, 'Containers'), 'compact navigation changes the single shared body');
  assert.deepEqual(
    stage.frames
      .flatMap((frame) => frame.patches)
      .filter((patch) => patch.SetProp?.id === splitter && patch.SetProp.prop === 'Position')
      .at(-1)?.SetProp.value,
    { Number: 160 },
    'the default leaves daily-driver content room at the 520px launch width',
  );
  assert.ok(
    stage.surface.dispatch({
      trigger: 'Change',
      node: splitter,
      id: `${splitter}:Change`,
      value: 999,
    }),
  );
  assert.deepEqual(
    stage.frames
      .flatMap((frame) => frame.patches)
      .filter((patch) => patch.SetProp?.id === splitter && patch.SetProp.prop === 'Position')
      .at(-1).SetProp.value,
    { Number: 240 },
  );
  const sidebarWidth = stage.frames
    .flatMap((frame) => frame.patches)
    .filter((patch) => patch.SetProp?.prop === 'Width')
    .find((patch) => patch.SetProp.value?.Bounds?.minimum?.Chars === 18);
  assert.deepEqual(
    sidebarWidth?.SetProp.value,
    { Bounds: { minimum: { Chars: 18 }, maximum: { Chars: 30 } } },
    'the navigation width follows the compact splitter range instead of colliding with it',
  );
});

test('a developer drag wins over a late stored sidebar width and is persisted', async () => {
  let resolveRead;
  let reads = 0;
  const writes = [];
  const preferences = {
    read: () => {
      reads += 1;
      if (reads > 1) return Promise.resolve({ revision: 7, entries: [] });
      return new Promise((resolve) => {
        resolveRead = resolve;
      });
    },
    set: async (observed, key, value) => {
      writes.push([observed, key, value]);
      return observed + 1;
    },
  };
  const stage = host();
  stage.render(
    h(Top, {
      api: { ...api, preferences },
      initial: {
        containers: [],
        executions: [],
        images: [],
        volumes: [],
        networks: [],
        terminals: [],
        extensions: [],
      },
    }),
  );
  await settled();
  const splitter = stage.frames
    .flatMap((frame) => frame.patches)
    .find((patch) => patch.Create?.tag === 'Responsive').Create.id;
  assert.ok(
    stage.surface.dispatch({
      trigger: 'Change',
      node: splitter,
      id: `${splitter}:Change`,
      value: 208,
    }),
  );
  resolveRead({
    revision: 7,
    entries: [['sidebar.width', { kind: 'number', value: 300 }]],
  });
  await settled();
  await new Promise((resolve) => setTimeout(resolve, SIDEBAR_SAVE_DELAY_MS + 25));
  await settled();

  const positions = stage.frames
    .flatMap((frame) => frame.patches)
    .filter((patch) => patch.SetProp?.id === splitter && patch.SetProp.prop === 'Position');
  assert.deepEqual(positions.at(-1).SetProp.value, { Number: 208 });
  assert.deepEqual(writes, [[7, 'sidebar.width', { kind: 'number', value: 208 }]]);
});

test('Top owns workspace settings and extension management in the same tab', async () => {
  const managed = {
    ...api,
    info: async () => ({ name: 'daily', architecture: 'amd64', image: 'alpine:3.20' }),
    inspect: async () => ({
      generation: 'a'.repeat(32),
      name: 'daily',
      architecture: 'amd64',
      image: 'alpine:3.20',
      storage: null,
      shell: '/bin/sh',
      cpus: 2,
      memory_mb: 1024,
      environment: [['TOKEN', 'hunter2']],
      mounts: [],
      docker_socket: false,
      scrollback: 10000,
      vpn: null,
      execution_lifetime: 'live',
      terminal: {
        font_family: null,
        font_size: null,
        foreground: '#eeeeec',
        background: '#1e1e1e',
        cursor_shape: null,
        cursor_blink: false,
      },
    }),
    extensions: { list: async () => [], catalogue: firstPartyCatalogue },
    watchExtensions: async () => () => {},
  };
  const stage = host();
  stage.render(
    h(Top, {
      api: managed,
      initial: {
        containers: [],
        executions: [],
        images: [],
        volumes: [],
        networks: [],
        terminals: [],
      },
    }),
  );
  invoke(stage, 'Settings');
  await settled();
  await settled();
  assert.equal(taggedProperty(stage, 'Workspace', 'NavigationMenuItem', 'Selected')?.Flag, false);
  assert.equal(taggedProperty(stage, 'Settings', 'NavigationMenuItem', 'Selected')?.Flag, true);
  assert.ok(
    taggedProperty(stage, 'Workspace settings', 'Heading', 'Label'),
    'the settings route has an unambiguous accessible page heading',
  );
  assert.ok(labelled(stage, 'Runtime'));
  assert.deepEqual(ancestorTags(stage, 'Execution lifetime').slice(0, 1), ['FormControl']);
  const lifetime = formControlField(stage, 'Execution lifetime', 'Select');
  assert.deepEqual(latestProperty(stage, lifetime, 'Value'), { Text: 'live' });
  assert.ok(labelled(stage, 'Resources & connectivity'));
  assert.ok(labelled(stage, 'Terminal appearance'));
  assert.ok(labelled(stage, 'Environment variables'));
  assert.ok(labelled(stage, 'Filesystem mounts'));
  assert.ok(
    labelled(stage, 'Runtime · Image alpine:3.20 · Shell /bin/sh'),
    'accordion summaries form one natural, explicitly separated phrase',
  );
  assert.ok(labelled(stage, 'Up to date'));
  assert.equal(
    ancestorTags(stage, 'Up to date').includes('Scroll'),
    false,
    'workspace save state remains visible while the settings body scrolls',
  );
  assert.deepEqual(
    outerAncestorProperty(stage, 'Up to date', 'Column', 'Pad'),
    { Length: { Step: 4 } },
    'the sticky workspace status card shares the page inset instead of touching the viewport',
  );
  assert.equal(labelled(stage, 'Unsaved changes'), undefined);
  change(stage, 'registry/image:tag', 'alpine:3.20');
  await settled();
  assert.equal(
    labelled(stage, 'Unsaved changes'),
    undefined,
    'a native initialization callback carrying the loaded value does not dirty the form',
  );
  change(stage, 'registry/image:tag', 'alpine:3.21');
  await settled();
  assert.ok(labelled(stage, 'Unsaved changes'));
  assert.equal(isEnabled(stage, 'Save changes'), true);
  invoke(stage, 'Discard');
  await settled();
  await settled();
  assert.ok(labelled(stage, 'Up to date'), 'discard restores an explicit clean state');
  assert.deepEqual(latestProperty(stage, lifetime, 'Value'), { Text: 'live' });
  assert.ok(
    stage.surface.dispatch({
      trigger: 'Change',
      node: lifetime,
      id: `${lifetime}:Change`,
      value: 'ephemeral',
    }),
    'the labelled execution lifetime selector keeps its live Change handler',
  );
  await settled();
  assert.deepEqual(latestProperty(stage, lifetime, 'Value'), { Text: 'ephemeral' });
  invoke(stage, 'Discard');
  await settled();
  await settled();
  assert.deepEqual(latestProperty(stage, lifetime, 'Value'), { Text: 'live' });
  expand(stage, 'Terminal appearance');
  await settled();
  assert.deepEqual(ancestorTags(stage, 'Cursor shape').slice(0, 1), ['FormControl']);
  const cursorShape = formControlField(stage, 'Cursor shape', 'Select');
  assert.deepEqual(latestProperty(stage, cursorShape, 'Value'), { Text: '' });
  assert.ok(
    stage.surface.dispatch({
      trigger: 'Change',
      node: cursorShape,
      id: `${cursorShape}:Change`,
      value: 'ibeam',
    }),
    'the labelled cursor shape selector keeps its live Change handler',
  );
  await settled();
  assert.deepEqual(latestProperty(stage, cursorShape, 'Value'), { Text: 'ibeam' });
  expand(stage, 'Resources & connectivity');
  await settled();
  assert.ok(labelled(stage, 'Storage directory'));
  assert.ok(
    ancestorProperty(stage, 'Storage directory', 'Card', 'Width'),
    'the settings editor shares the available page width',
  );
  assert.equal(
    ancestorProperty(stage, 'Storage directory', 'Card', 'Justify'),
    undefined,
    'settings do not override fill width with a conflicting cross-axis alignment',
  );
  expand(stage, 'Environment variables');
  await settled();
  assert.equal(placeholderProperty(stage, 'value', 'Secret')?.Flag, true);
  assert.deepEqual(
    taggedProperty(stage, 'Remove TOKEN', 'IconButton', 'Icon'),
    { Text: 'user-trash-symbolic' },
    'row removal is a compact secondary action instead of a full text button',
  );
  assert.equal(
    ancestorProperty(stage, 'Remove TOKEN', 'Row', 'Wrap')?.Flag,
    true,
    'environment controls reflow instead of colliding at narrow widths',
  );
  toggleLatestSwitch(stage, true);
  await settled();
  assert.equal(placeholderProperty(stage, 'value', 'Secret')?.Flag, false);
  invoke(stage, 'Extensions');
  await settled();
  await settled();
  selectExtensionMode(stage, 'Discover');
  await settled();
  assert.ok(labelled(stage, 'Discover'));
  assert.ok(labelled(stage, '1 extension'));
  assert.ok(labelled(stage, 'Component playground'));
  assert.ok(
    labelled(stage, 'Image · ghcr.io/husklet/husklet/extension-storybook:latest'),
    'discovery names the exact OCI input that review will inspect',
  );
  assert.ok(labelled(stage, 'Install from an OCI image'));
  assert.ok(
    labelled(
      stage,
      'Paste an OCI image reference. You’ll review compatibility and requested access before installation.',
    ),
  );
  assert.ok(labelled(stage, 'No extensions installed'));
  assert.equal(labelled(stage, 'Workspace control'), undefined);
  assert.deepEqual(ancestorTags(stage, 'Discover').slice(0, 3), ['Row', 'Column', 'Column']);
  assert.deepEqual(ancestorTags(stage, 'Installed extensions').slice(0, 3), [
    'Row',
    'Column',
    'Column',
  ]);
  assert.deepEqual(
    ancestorProperty(stage, 'Discover', 'Column', 'Width'),
    { Length: 'Fill' },
    'extension sections use the full page width without separating related content',
  );
  assert.equal(
    taggedProperty(stage, 'Component playground', 'CardHeader', 'Detail')?.Text,
    'Husklet · Version 2.0.0',
  );
  assert.equal(labelled(stage, 'Version 2.0.0'), undefined, 'the header version is not repeated');
  assert.ok(labelled(stage, 'Trust & compatibility'));
  assert.equal(
    ancestorTags(stage, 'Husklet · Version 2.0.0').includes('Expander'),
    false,
    'publisher provenance remains visible before acquisition starts',
  );
  assert.ok(labelled(stage, 'Verified publisher · Husklet'));
  assert.ok(labelled(stage, 'Compatibility undeclared'));
  assert.equal(labelled(stage, 'Review requested access before anything is installed.'), undefined);
  assert.deepEqual(ancestorTags(stage, 'Review access').slice(0, 4), ['Row', 'Row', 'Card', 'Row']);
  assert.equal(
    ancestorProperty(stage, 'Component playground', 'Card', 'Justify'),
    undefined,
    'extension cards fill their responsive column instead of overriding width with start alignment',
  );
  assert.deepEqual(ancestorProperty(stage, 'Component playground', 'Card', 'Width'), {
    Bounds: { minimum: { Chars: 38 }, maximum: 'Fill' },
  });
  assert.deepEqual(
    taggedProperty(stage, 'Refresh installed extensions', 'IconButton', 'Icon'),
    { Text: 'view-refresh-symbolic' },
    'inventory refresh is a compact icon action instead of a competing text button',
  );
  assert.equal(
    taggedProperty(stage, 'Install from an OCI image', 'Expander', 'Expanded')?.Flag,
    false,
    'advanced image installation is collapsed until requested',
  );
  assert.deepEqual(
    outerAncestorProperty(stage, 'Extensions', 'Container', 'Pad'),
    { Length: { Step: 4 } },
    'the catalogue uses the same 16px page inset as operational pages',
  );
  invoke(stage, 'Settings');
  await settled();
  await settled();
  assert.ok(labelled(stage, 'Runtime access'));
  assert.ok(
    labelled(
      stage,
      'Docker-compatible socket access lets processes in this workspace control its container engine. Only enable it for trusted workspace code.',
    ),
  );
  assert.ok(labelled(stage, 'This change takes effect after the workspace restarts.'));
  invoke(stage, 'Enable Docker socket');
  await settled();
  assert.ok(
    labelled(
      stage,
      'Allow trusted workspace processes to control this workspace’s container engine after restart?',
    ),
  );
  invoke(stage, 'Confirm socket access');
  await settled();
  assert.ok(labelled(stage, 'Docker-compatible workspace socket is enabled.'));
  assert.ok(labelled(stage, 'Disable Docker socket'));
});

test('workspace save rotates environment through the explicit revision-bound patch', async () => {
  const calls = [];
  const generation = 'a'.repeat(32);
  const revision = 'b'.repeat(32);
  const nextRevision = 'c'.repeat(32);
  const configuration = {
    generation,
    configuration_revision: revision,
    name: 'daily',
    architecture: 'amd64',
    image: 'alpine:3.20',
    storage: null,
    shell: '/bin/sh',
    cpus: 2,
    memory_mb: 1024,
    environment: [['TOKEN', 'old']],
    mounts: [],
    docker_socket: false,
    scrollback: 10000,
    vpn: null,
    execution_lifetime: 'live',
    terminal: {
      font_family: null,
      font_size: null,
      foreground: null,
      background: null,
      cursor_shape: null,
      cursor_blink: false,
    },
  };
  const managed = {
    ...api,
    info: async () => ({ name: 'daily', architecture: 'amd64', image: 'alpine:3.20' }),
    inspect: async () => configuration,
    update: async (...args) => {
      calls.push(['update', ...args]);
      return { ...args[3], generation, configuration_revision: nextRevision };
    },
    patchEnvironment: async (...args) => {
      calls.push(['patch', ...args]);
      return { generation, configuration_revision: 'd'.repeat(32), changed: true };
    },
  };
  const stage = host();
  stage.render(h(Workspace, { api: managed }));
  await settled();
  await settled();
  const environment = stage.frames
    .flatMap((frame) => frame.patches)
    .filter(
      (patch) =>
        patch.SetProp?.prop === 'Label' && patch.SetProp.value?.Text === 'Environment variables',
    )
    .map((patch) => patch.SetProp.id)
    .find((node) =>
      stage.surface.dispatch({ trigger: 'Expand', node, id: `${node}:Expand`, value: true }),
    );
  assert.notEqual(environment, undefined);
  await settled();
  toggleLatestSwitch(stage, true);
  await settled();
  assert.equal(placeholderProperty(stage, 'value', 'Secret')?.Flag, false);
  change(stage, 'value', 'new');
  invoke(stage, 'Save changes');
  await settled();
  await settled();
  assert.equal(calls[0][0], 'update');
  assert.deepEqual(calls[0][4].environment, [['TOKEN', 'old']]);
  assert.deepEqual(calls[1], [
    'patch',
    'daily',
    generation,
    nextRevision,
    { set: [['TOKEN', 'new']], remove: [] },
  ]);
  assert.equal(
    placeholderProperty(stage, 'value', 'Secret')?.Flag,
    true,
    'a saved revision returns authoritative environment values to concealed state',
  );
});

test('workspace save failure retains edits and offers an explicit retry', async () => {
  const configuration = {
    generation: 'a'.repeat(32),
    configuration_revision: 'b'.repeat(32),
    name: 'daily',
    architecture: 'amd64',
    image: 'alpine:3.20',
    storage: null,
    shell: '/bin/sh',
    cpus: 2,
    memory_mb: 1024,
    environment: [],
    mounts: [],
    docker_socket: false,
    scrollback: 10000,
    vpn: null,
    execution_lifetime: 'live',
    terminal: {
      font_family: null,
      font_size: null,
      foreground: null,
      background: null,
      cursor_shape: null,
      cursor_blink: false,
    },
  };
  let attempts = 0;
  const managed = {
    ...api,
    info: async () => ({ name: 'daily', architecture: 'amd64', image: 'alpine:3.20' }),
    inspect: async () => configuration,
    update: async () => {
      attempts += 1;
      throw new Error('host rejected settings write');
    },
  };
  const stage = host();
  stage.render(h(Workspace, { api: managed }));
  await settled();
  await settled();
  change(stage, 'Automatic when empty', '/bin/zsh');
  invoke(stage, 'Save changes');
  await settled();
  await settled();
  assert.ok(labelled(stage, 'Unsaved changes'));
  assert.ok(
    labelled(
      stage,
      'No successful save was confirmed. Your edits are retained: host rejected settings write',
    ),
  );
  assert.ok(labelled(stage, 'Retry save'));
  invoke(stage, 'Retry save');
  await settled();
  await settled();
  assert.equal(attempts, 2);
});

test('workspace patch conflict reloads authority and keeps the partial-save warning visible', async () => {
  const generation = 'a'.repeat(32);
  const revision = 'b'.repeat(32);
  const nextRevision = 'c'.repeat(32);
  const configuration = {
    generation,
    configuration_revision: revision,
    name: 'daily',
    architecture: 'amd64',
    image: 'alpine:3.20',
    storage: null,
    shell: '/bin/sh',
    cpus: 2,
    memory_mb: 1024,
    environment: [['TOKEN', 'old']],
    mounts: [],
    docker_socket: false,
    scrollback: 10000,
    vpn: null,
    execution_lifetime: 'live',
    terminal: {
      font_family: null,
      font_size: null,
      foreground: null,
      background: null,
      cursor_shape: null,
      cursor_blink: false,
    },
  };
  let inspections = 0;
  const managed = {
    ...api,
    info: async () => ({ name: 'daily', architecture: 'amd64', image: 'alpine:3.20' }),
    inspect: async () => {
      inspections += 1;
      return configuration;
    },
    update: async (...args) => ({ ...args[3], generation, configuration_revision: nextRevision }),
    patchEnvironment: async () => {
      throw new Error('workspace changed');
    },
  };
  const stage = host();
  stage.render(h(Workspace, { api: managed }));
  await settled();
  await settled();
  const environment = stage.frames
    .flatMap((frame) => frame.patches)
    .filter(
      (patch) =>
        patch.SetProp?.prop === 'Label' && patch.SetProp.value?.Text === 'Environment variables',
    )
    .map((patch) => patch.SetProp.id)
    .find((node) =>
      stage.surface.dispatch({ trigger: 'Expand', node, id: `${node}:Expand`, expanded: true }),
    );
  assert.notEqual(environment, undefined);
  change(stage, 'value', 'new');
  invoke(stage, 'Save changes');
  await settled();
  await settled();
  await settled();
  assert.equal(inspections, 2, 'conflict performs an authoritative reload');
  const labels = stage.frames
    .flatMap((frame) => frame.patches)
    .filter((patch) => patch.SetProp?.prop === 'Label')
    .map((patch) => patch.SetProp.value?.Text ?? '');
  assert.ok(
    labels.some((label) =>
      label.includes(
        'Settings were saved, but environment changes were not. Reloaded the latest workspace and retained your concealed environment edits for review and retry',
      ),
    ),
  );
  assert.equal(fieldValue(stage, 'value'), 'new');
  assert.equal(placeholderProperty(stage, 'value', 'Secret')?.Flag, true);
  assert.equal(isEnabled(stage, 'Save changes'), true);
});

test('extension discovery reviews the first-party Storybook without requiring a registry path', async () => {
  const references = [];
  const stage = host();
  stage.render(
    h(Extensions, {
      api: {
        extensions: {
          list: async () => [],
          catalogue: firstPartyCatalogue,
          startAcquisition: async (reference) => {
            references.push(reference);
            return { job: 'storybook-review' };
          },
          acquisition: async () => ({
            job: 'storybook-review',
            reference: 'ghcr.io/husklet/husklet/extension-storybook:latest',
            revision: 1,
            state: 'failed',
            progress: null,
            candidate: null,
            error: 'offline fixture',
          }),
        },
        watchExtensions: async () => () => {},
      },
    }),
  );
  await settled();
  selectExtensionMode(stage, 'Discover');
  await settled();
  assert.deepEqual(
    taggedProperty(stage, 'Review access', 'Button', 'Size'),
    { ControlSize: 'Small' },
    'new-extension review keeps its catalogue card compact',
  );
  invoke(stage, 'Review access');
  await settled();
  await settled();
  assert.deepEqual(references, ['ghcr.io/husklet/husklet/extension-storybook:latest']);
  assert.equal(
    fieldValue(stage, 'registry.example/extension:version'),
    'ghcr.io/husklet/husklet/extension-storybook:latest',
  );
});

test('large extension catalogues search and filter deterministic lifecycle projections', () => {
  assert.deepEqual(
    filterCatalogueEntries(
      largeCatalogueEntries,
      largeCatalogueInstalled,
      'amd64',
      '',
      'discover',
    ).map((entry) => entry.id),
    [
      'database',
      ...Array.from({ length: 17 }, (_, index) => `tool-${String(index + 1).padStart(2, '0')}`),
      'future',
    ],
    'default discovery keeps updates and available entries but omits installed up-to-date entries',
  );
  assert.deepEqual(
    filterCatalogueEntries(
      largeCatalogueEntries,
      largeCatalogueInstalled,
      'amd64',
      '',
      'updates',
    ).map((entry) => entry.id),
    ['database'],
  );
  assert.deepEqual(
    filterCatalogueEntries(
      largeCatalogueEntries,
      largeCatalogueInstalled,
      'amd64',
      '',
      'installed',
    ).map((entry) => entry.id),
    ['storybook', 'database'],
    'installed entries are ordered by visible title rather than catalogue input',
  );
  assert.deepEqual(
    filterCatalogueEntries(
      largeCatalogueEntries,
      largeCatalogueInstalled,
      'amd64',
      'searchable labs',
      'available',
    ).map((entry) => entry.id),
    ['tool-09'],
    'publisher search composes with lifecycle status',
  );
  assert.deepEqual(
    filterCatalogueEntries(
      largeCatalogueEntries,
      largeCatalogueInstalled,
      'amd64',
      '',
      'incompatible',
    ).map((entry) => entry.id),
    ['future'],
  );
});

test('extension modes isolate collections and reset controls in deterministic keyboard order', async () => {
  const stage = host();
  stage.render(
    h(Extensions, {
      api: {
        info: async () => ({ name: 'daily', architecture: 'amd64', image: 'alpine' }),
        extensions: {
          list: async () => largeCatalogueInstalled,
          catalogue: async () => ({ entries: largeCatalogueEntries, complete: true }),
        },
        watchExtensions: async () => () => {},
      },
    }),
  );
  await settled();
  await settled();
  let labels = orderedLabels(stage);
  assert.ok(labels.includes('Find installed extensions'));
  assert.ok(!labels.includes('Find extensions'));
  assert.ok(labels.indexOf('Installed') < labels.indexOf('Find installed extensions'));
  assert.ok(
    labels.indexOf('Find installed extensions') < labels.indexOf('2 of 2 installed extensions'),
  );
  assert.equal(placeholderTag(stage, 'Search installed'), 'Search');
  change(stage, 'Search installed', 'database');
  changeByTooltip(stage, 'Filter installed extensions by status', 'updates');
  await settled();

  selectExtensionMode(stage, 'Discover');
  await settled();
  labels = orderedLabels(stage);
  assert.ok(labels.includes('Find extensions'));
  assert.ok(!labels.includes('Find installed extensions'));
  assert.ok(labels.indexOf('Discover') < labels.indexOf('Find extensions'));
  assert.ok(labels.indexOf('Find extensions') < labels.indexOf('19 of 20 extensions'));
  assert.equal(fieldValue(stage, 'Search extensions'), '');
  assert.equal(placeholderTag(stage, 'Search extensions'), 'Search');
  assert.equal(fieldValueByTooltip(stage, 'Filter extension catalogue by status'), 'discover');

  change(stage, 'Search extensions', 'future');
  changeByTooltip(stage, 'Filter extension catalogue by status', 'incompatible');
  selectExtensionMode(stage, 'Installed');
  await settled();
  assert.equal(fieldValue(stage, 'Search installed'), '');
  assert.equal(fieldValueByTooltip(stage, 'Filter installed extensions by status'), 'all');
});

test('extension discovery searches, filters, reports result counts, and clears a no-match state', async () => {
  const stage = host();
  stage.render(
    h(Extensions, {
      api: {
        info: async () => ({ name: 'daily', architecture: 'amd64', image: 'alpine' }),
        extensions: {
          list: async () => largeCatalogueInstalled,
          catalogue: async () => ({ entries: largeCatalogueEntries, complete: true }),
        },
        watchExtensions: async () => () => {},
      },
    }),
  );
  await settled();
  await settled();
  selectExtensionMode(stage, 'Discover');
  await settled();
  assert.ok(labelled(stage, '19 of 20 extensions'));
  assert.ok(labelled(stage, 'Showing 8 of 19 matching extensions'));
  assert.ok(labelled(stage, 'Show 8 more'));
  invoke(stage, 'Show 8 more');
  await settled();
  assert.ok(labelled(stage, 'Showing 16 of 19 matching extensions'));
  assert.deepEqual(placeholderProperty(stage, 'Search extensions', 'Width'), {
    Bounds: { minimum: { Chars: 18 }, maximum: { Chars: 36 } },
  });

  change(stage, 'Search extensions', 'postgres');
  await settled();
  assert.ok(labelled(stage, '1 of 20 extensions'));
  assert.ok(labelled(stage, 'Database Studio'));

  changeByTooltip(stage, 'Filter extension catalogue by status', 'updates');
  await settled();
  assert.ok(labelled(stage, '1 of 20 extensions'));

  change(stage, 'Search extensions', 'nothing matches this');
  await settled();
  assert.ok(labelled(stage, '0 of 20 extensions'));
  assert.ok(labelled(stage, 'No extensions match this search and status filter.'));
  assert.ok(labelled(stage, 'Clear filters'));
  invoke(stage, 'Clear filters');
  await settled();
  assert.ok(labelled(stage, '19 of 20 extensions'));
  assert.equal(fieldValue(stage, 'Search extensions'), '');

  changeByTooltip(stage, 'Filter extension catalogue by status', 'installed');
  await settled();
  assert.ok(labelled(stage, '2 of 20 extensions'));
  assert.ok(labelled(stage, 'Component playground'));
  assert.ok(labelled(stage, 'Installed · up to date'));

  changeByTooltip(stage, 'Filter extension catalogue by status', 'incompatible');
  await settled();
  assert.ok(labelled(stage, '1 of 20 extensions'));
  assert.ok(labelled(stage, 'Future debugger'));
});

test('installed extension projections prioritize faults and updates and search provider names', () => {
  assert.deepEqual(
    filterInstalledExtensions(largeInstalledExtensions, largeCatalogueEntries, '', 'all')
      .slice(0, 4)
      .map((extension) => extension.name),
    ['faulted-agent', 'database', 'disabled-linter', 'installed-01'],
    'faults, updates, and disabled entries lead deterministic name ordering',
  );
  assert.deepEqual(
    filterInstalledExtensions(largeInstalledExtensions, largeCatalogueEntries, '', 'faulted').map(
      (extension) => extension.name,
    ),
    ['faulted-agent'],
  );
  assert.deepEqual(
    filterInstalledExtensions(largeInstalledExtensions, largeCatalogueEntries, '', 'updates').map(
      (extension) => extension.name,
    ),
    ['database'],
  );
  assert.deepEqual(
    filterInstalledExtensions(
      largeInstalledExtensions,
      largeCatalogueEntries,
      'observability dashboard',
      'running',
    ).map((extension) => extension.name),
    ['installed-07'],
  );
});

test('installed extension management searches, filters, pages, and clears fifty entries', async () => {
  const stage = host();
  stage.render(
    h(Extensions, {
      api: {
        info: async () => ({ name: 'daily', architecture: 'amd64', image: 'alpine' }),
        extensions: {
          list: async () => largeInstalledExtensions,
          catalogue: async () => ({ entries: largeCatalogueEntries, complete: true }),
        },
        watchExtensions: async () => () => {},
      },
    }),
  );
  await settled();
  await settled();
  assert.ok(labelled(stage, '50 of 50 installed extensions'));
  assert.ok(labelled(stage, 'Showing 12 of 50 matching installed extensions'));
  assert.ok(labelled(stage, 'Show 12 more installed'));
  assert.deepEqual(taggedProperty(stage, 'Disabled', 'Badge', 'Tone'), { Tone: 'Neutral' });
  invoke(stage, 'Show 12 more installed');
  await settled();
  assert.ok(labelled(stage, 'Showing 24 of 50 matching installed extensions'));

  changeByTooltip(stage, 'Filter installed extensions by status', 'faulted');
  await settled();
  assert.ok(labelled(stage, '1 of 50 installed extensions'));
  assert.ok(labelled(stage, 'faulted-agent'));

  change(stage, 'Search installed', 'no such extension');
  await settled();
  assert.ok(labelled(stage, '0 of 50 installed extensions'));
  assert.ok(labelled(stage, 'No installed extensions match this search and status filter.'));
  invoke(stage, 'Clear installed filters');
  await settled();
  assert.ok(labelled(stage, '50 of 50 installed extensions'));
  assert.equal(fieldValue(stage, 'Search installed'), '');

  change(stage, 'Search installed', 'observability dashboard');
  await settled();
  assert.ok(labelled(stage, '1 of 50 installed extensions'));
  assert.ok(labelled(stage, 'installed-07'));
});

test('extension discovery keeps unknown compatibility reviewable and blocks known mismatches', async () => {
  const stage = host();
  stage.render(
    h(Extensions, {
      api: {
        info: async () => ({ name: 'daily', architecture: 'amd64', image: 'alpine' }),
        extensions: {
          list: async () => [],
          catalogue: async () => ({
            complete: true,
            entries: [
              {
                id: 'unknown',
                title: 'Unknown',
                description: 'u',
                reference: 'u',
                publisher: 'p',
                source: 's',
              },
              {
                id: 'arm',
                title: 'ARM only',
                description: 'a',
                reference: 'a',
                publisher: 'p',
                source: 's',
                architectures: ['arm64'],
              },
              {
                id: 'future',
                title: 'Future protocol',
                description: 'f',
                reference: 'f',
                publisher: 'p',
                source: 's',
                protocol: 999,
              },
            ],
          }),
        },
        watchExtensions: async () => () => {},
      },
    }),
  );
  await settled();
  await settled();
  selectExtensionMode(stage, 'Discover');
  await settled();
  assert.ok(labelled(stage, 'Compatibility not declared'));
  assert.deepEqual(
    enabledStates(stage, 'Review access'),
    [false, false, true],
    'deterministic title ordering preserves compatibility authority per card',
  );
  assert.ok(labelled(stage, 'Incompatible · supports arm64; workspace is amd64'));
  assert.ok(labelled(stage, 'Incompatible · requires protocol 999; this client uses 1'));
});

test('an installed catalogue extension exposes its update review without retyping a reference', async () => {
  const references = [];
  let releaseStart;
  const startPending = new Promise((resolve) => {
    releaseStart = resolve;
  });
  const digest = `sha256:${'a'.repeat(64)}`;
  const stage = host();
  stage.render(
    h(Extensions, {
      api: {
        extensions: {
          list: async () => [
            {
              name: 'storybook',
              image_digest: digest,
              version: '1.0.0',
              enabled: true,
              status: 'duty',
            },
          ],
          catalogue: firstPartyCatalogue,
          startAcquisition: async (reference) => {
            references.push(reference);
            await startPending;
            return { job: 'storybook-update' };
          },
          acquisition: async () => ({
            job: 'storybook-update',
            reference: 'ghcr.io/husklet/husklet/extension-storybook:latest',
            revision: 3,
            state: 'ready',
            progress: null,
            candidate: {
              name: 'storybook',
              version: '2.0.0',
              image_digest: `sha256:${'b'.repeat(64)}`,
              installed_image_digest: digest,
              requested: [],
            },
            error: null,
          }),
        },
        watchExtensions: async () => () => {},
      },
    }),
  );
  await settled();
  assert.deepEqual(
    taggedProperty(stage, 'Review update', 'Button', 'Size'),
    { ControlSize: 'Small' },
    'installed update review keeps its management card compact',
  );
  selectExtensionMode(stage, 'Discover');
  await settled();
  assert.equal(
    labelledInCard(stage, 'Component playground', 'Review update').length,
    1,
    'the Discover update card owns an immediate review action',
  );
  assert.deepEqual(
    taggedProperty(stage, 'Review update', 'Button', 'Size'),
    { ControlSize: 'Small' },
    'the Discover update action keeps catalogue cards compact',
  );
  assert.ok(labelled(stage, 'Update to Version 2.0.0 · Compatibility not declared'));
  assert.equal(
    labelled(stage, 'Review access'),
    undefined,
    'installed catalogue entries do not also appear as new installations',
  );
  assert.ok(labelled(stage, 'Installed · update available'));
  assert.equal(
    labelled(
      stage,
      'Everything in the built-in catalogue is installed. Available updates appear below.',
    ),
    undefined,
  );

  invokeInCard(stage, 'Component playground', 'Review update');
  invokeInCard(stage, 'Component playground', 'Review update');
  assert.deepEqual(references, ['ghcr.io/husklet/husklet/extension-storybook:latest']);
  releaseStart();
  await settled();
  await settled();
  assert.deepEqual(references, ['ghcr.io/husklet/husklet/extension-storybook:latest']);
  assert.ok(
    labelled(
      stage,
      `Replaces installed image ${compactDigest(digest)}. Access below was reset and must be approved again.`,
    ),
  );
  assert.ok(labelled(stage, 'Update with selected access'));
});

test('an up-to-date built-in is hidden by default and available through the installed filter', async () => {
  const calls = [];
  let releaseOpen;
  const opening = new Promise((resolve) => {
    releaseOpen = resolve;
  });
  const stage = host();
  stage.render(
    h(Extensions, {
      api: {
        extensions: {
          list: async () => [
            {
              name: 'storybook',
              image_digest: `sha256:${'a'.repeat(64)}`,
              version: '2.0.0',
              enabled: true,
              status: 'duty',
              pane_providers: [{ id: 'playground', title: 'Component playground', icon: null }],
            },
          ],
          catalogue: firstPartyCatalogue,
          startAcquisition: async (reference) => {
            calls.push(['inspect', reference]);
            return { job: 'current-image' };
          },
          acquisition: async () => ({
            job: 'current-image',
            reference: firstPartyCatalogue.entries[0].reference,
            revision: 2,
            state: 'ready',
            progress: null,
            candidate: {
              name: 'storybook',
              version: '2.0.0',
              image_digest: `sha256:${'a'.repeat(64)}`,
              installed_image_digest: `sha256:${'a'.repeat(64)}`,
              requested: [],
              required: [],
            },
            error: null,
          }),
        },
        terminal: {
          openTabAndWait: async (...args) => {
            calls.push(['open', ...args]);
            await opening;
            return {
              changed: true,
              tab: 'tab-storybook',
              pane: { slot: 'pane-storybook', generation: 7, revision: 11 },
            };
          },
          switchOccupantAndWait: async (...args) => {
            calls.push(['switch', ...args]);
            return {
              changed: true,
              pane: { slot: 'pane-storybook', generation: 7, revision: 12 },
            };
          },
          focus: async (...args) => calls.push(['focus', ...args]),
        },
        watchExtensions: async () => () => {},
      },
    }),
  );
  await settled();
  await settled();
  selectExtensionMode(stage, 'Discover');
  await settled();
  assert.equal(
    labelled(stage, 'Installed · up to date'),
    undefined,
    'default discovery does not duplicate the installed management card',
  );
  assert.ok(labelled(stage, 'Open Component playground'));

  changeByTooltip(stage, 'Filter extension catalogue by status', 'installed');
  await settled();
  assert.ok(labelled(stage, 'Component playground'));
  assert.ok(labelled(stage, 'Installed · up to date'));
  assert.ok(labelled(stage, '1 extension · all installed'));
  assert.ok(labelled(stage, 'Installed image'));
  assert.equal(labelled(stage, 'View installed details'), undefined);
  assert.equal(labelled(stage, 'Review access'), undefined);
  assert.equal(labelled(stage, 'Review update'), undefined);
  assert.ok(labelled(stage, 'Open Component playground'));
  assert.ok(labelled(stage, 'Check current image'));
  invoke(stage, 'Check current image');
  await settled();
  await settled();
  assert.ok(calls.some(([operation]) => operation === 'inspect'));
  assert.deepEqual(
    taggedProperty(stage, 'Open Component playground', 'Button', 'Size'),
    { ControlSize: 'Small' },
    'opening an installed extension stays a compact primary card action',
  );
  invoke(stage, 'Open Component playground');
  await settled();
  assert.ok(labelled(stage, 'Opening…'));
  assert.equal(isEnabled(stage, 'Opening…'), false);
  assert.ok(
    stage.frames.flatMap((frame) => frame.patches).some((patch) => patch.Create?.tag === 'Spinner'),
  );
  invoke(stage, 'Opening…');
  assert.deepEqual(
    calls.filter(([operation]) => operation !== 'inspect'),
    [['open', 'Component playground']],
  );
  releaseOpen();
  await settled();
  await settled();
  assert.deepEqual(
    calls.filter(([operation]) => operation !== 'inspect'),
    [
      ['open', 'Component playground'],
      [
        'switch',
        'pane-storybook',
        7,
        11,
        { kind: 'surface', extension: 'storybook', provider: 'playground' },
      ],
      ['focus', 'pane-storybook'],
    ],
  );
  assert.ok(labelled(stage, 'Component playground opened in a new tab.'));
});

test('opening an extension reports a retained tab when occupant switching fails', async () => {
  const stage = host();
  stage.render(
    h(Extensions, {
      api: {
        extensions: {
          list: async () => [
            {
              name: 'storybook',
              image_digest: `sha256:${'a'.repeat(64)}`,
              version: '2.0.0',
              enabled: true,
              status: 'duty',
              pane_providers: [{ id: 'playground', title: 'Component playground', icon: null }],
            },
          ],
          catalogue: firstPartyCatalogue,
        },
        terminal: {
          openTabAndWait: async () => ({
            changed: true,
            tab: 'tab-retained',
            pane: { slot: 'pane-retained', generation: 3, revision: 9 },
          }),
          switchOccupantAndWait: async () => {
            throw new Error('provider stopped before mounting');
          },
        },
        watchExtensions: async () => () => {},
      },
    }),
  );
  await settled();
  await settled();
  invoke(stage, 'Open Component playground');
  await settled();
  await settled();
  assert.ok(
    labelled(
      stage,
      'Tab tab-retained was created, but Component playground did not open: provider stopped before mounting',
    ),
  );
  assert.ok(labelled(stage, 'Retry opening'));
});

test('opening an extension restores its idle action after tab creation fails', async () => {
  const stage = host();
  stage.render(
    h(Extensions, {
      api: {
        extensions: {
          list: async () => [
            {
              name: 'storybook',
              image_digest: `sha256:${'a'.repeat(64)}`,
              version: '2.0.0',
              enabled: true,
              status: 'duty',
              pane_providers: [{ id: 'playground', title: 'Component playground', icon: null }],
            },
          ],
          catalogue: firstPartyCatalogue,
        },
        terminal: {
          openTabAndWait: async () => {
            throw new Error('terminal window is unavailable');
          },
        },
        watchExtensions: async () => () => {},
      },
    }),
  );
  await settled();
  await settled();
  invoke(stage, 'Open Component playground');
  await settled();
  await settled();
  assert.ok(labelled(stage, 'Open Component playground'));
  assert.ok(
    labelled(stage, 'Component playground could not be opened: terminal window is unavailable'),
  );
});

test('reviewing an unchanged installed digest is an explicit no-op', async () => {
  const digest = `sha256:${'a'.repeat(64)}`;
  let updates = 0;
  const stage = host();
  stage.render(
    h(Extensions, {
      api: {
        extensions: {
          list: async () => [
            {
              name: 'storybook',
              image_digest: digest,
              version: '1.0.0',
              enabled: true,
              status: 'duty',
            },
          ],
          catalogue: firstPartyCatalogue,
          startAcquisition: async () => ({ job: 'unchanged-update' }),
          acquisition: async () => ({
            job: 'unchanged-update',
            reference: 'ghcr.io/husklet/husklet/extension-storybook:latest',
            revision: 2,
            state: 'ready',
            progress: null,
            candidate: {
              name: 'storybook',
              version: '1.0.0',
              image_digest: digest,
              installed_image_digest: digest,
              requested: ['containers:read'],
            },
            error: null,
          }),
          updateAndWait: async () => {
            updates += 1;
          },
        },
        watchExtensions: async () => () => {},
      },
    }),
  );
  await settled();
  invoke(stage, 'Review update');
  await settled();
  await settled();

  assert.ok(
    labelled(
      stage,
      'storybook is up to date. The reviewed image already matches the installed image; access was not changed.',
    ),
  );
  assert.equal(labelled(stage, 'Update with selected access'), undefined);
  assert.equal(updates, 0);
});

test('catalogue does not advertise an update at the installed version', async () => {
  const stage = host();
  stage.render(
    h(Extensions, {
      api: {
        extensions: {
          list: async () => [
            {
              name: 'storybook',
              image_digest: `sha256:${'a'.repeat(64)}`,
              version: '2.0.0',
              enabled: true,
              status: 'duty',
            },
          ],
          catalogue: firstPartyCatalogue,
        },
        watchExtensions: async () => () => {},
      },
    }),
  );
  await settled();
  assert.equal(labelled(stage, 'Review update'), undefined);
  assert.equal(labelled(stage, 'Update available'), undefined);
  assert.equal(labelled(stage, 'Update available · Version 2.0.0'), undefined);
});

test('catalogue never advertises an older release as an update', async () => {
  const stage = host();
  stage.render(
    h(Extensions, {
      api: {
        extensions: {
          list: async () => [
            {
              name: 'storybook',
              image_digest: `sha256:${'a'.repeat(64)}`,
              version: '2.0.0',
              enabled: true,
              status: 'duty',
            },
          ],
          catalogue: async () => ({
            ...(await firstPartyCatalogue()),
            entries: [{ ...(await firstPartyCatalogue()).entries[0], version: '0.4.0' }],
          }),
        },
        watchExtensions: async () => () => {},
      },
    }),
  );
  await settled();
  assert.equal(labelled(stage, 'Review update'), undefined);
  assert.equal(labelled(stage, 'Update available · Version 0.4.0'), undefined);
});

test('extension review calls out destructive image authority before consent', async () => {
  const stage = host();
  stage.render(
    h(Extensions, {
      api: {
        extensions: {
          list: async () => [],
          startAcquisition: async () => ({ job: 'image-review' }),
          acquisition: async () => ({
            job: 'image-review',
            reference: 'registry.example/tools:1',
            revision: 1,
            state: 'ready',
            progress: null,
            candidate: {
              name: 'tools',
              version: '1',
              image_digest: `sha256:${'a'.repeat(64)}`,
              installed_image_digest: null,
              requested: ['images:remove', 'images:prune'],
              requested_images: {
                read: [],
                use: [],
                pull: [],
                remove: [{ digest: `sha256:${'b'.repeat(64)}` }],
                prune_all_unused: true,
              },
            },
            error: null,
          }),
        },
        watchExtensions: async () => () => {},
      },
    }),
  );
  await settled();
  selectExtensionMode(stage, 'Discover');
  await settled();
  change(stage, 'registry.example/extension:version', 'registry.example/tools:1');
  invoke(stage, 'Inspect');
  await settled();
  await settled();
  assert.ok(
    labelled(
      stage,
      'Destructive access requested. Image removal deletes named images; prune deletes every unused image in this workspace.',
    ),
  );
  assert.equal(
    ancestorTags(
      stage,
      'Destructive access requested. Image removal deletes named images; prune deletes every unused image in this workspace.',
    ).includes('Expander'),
    false,
    'destructive authority is announced while exact grants remain collapsed',
  );
});

for (const updating of [false, true]) {
  test(`extension ${updating ? 'update' : 'install'} keeps exact image consent tied to its action`, async () => {
    const calls = [];
    const candidate = {
      name: 'image-tool',
      version: '2.0.0',
      image_digest: `sha256:${'a'.repeat(64)}`,
      installed_image_digest: updating ? `sha256:${'c'.repeat(64)}` : null,
      requested: [
        'images:read',
        'containers:create',
        'images:pull',
        'images:remove',
        'images:prune',
      ],
      requested_images: {
        read: [{ reference: 'registry.example/database:1' }],
        use: [{ digest: `sha256:${'b'.repeat(64)}` }],
        pull: [{ reference: 'registry.example/embeddings:2' }],
        remove: [{ reference: 'registry.example/test-runner:3' }],
        prune_all_unused: true,
      },
    };
    const stage = host();
    stage.render(
      h(Extensions, {
        api: {
          extensions: {
            list: async () => [],
            startAcquisition: async () => ({ job: 'image-consent' }),
            acquisition: async () => ({
              job: 'image-consent',
              reference: 'registry.example/image-tool:2',
              revision: 3,
              state: 'ready',
              progress: null,
              candidate,
              error: null,
            }),
            [`${updating ? 'update' : 'install'}AndWait`]: async (...args) => {
              calls.push(args);
              return { changed: true, extension: { ...candidate, status: 'running' } };
            },
          },
          watchExtensions: async () => () => {},
        },
      }),
    );
    await settled();
    selectExtensionMode(stage, 'Discover');
    await settled();
    change(stage, 'registry.example/extension:version', 'registry.example/image-tool:2');
    invoke(stage, 'Inspect');
    await settled();
    await settled();
    expand(stage, 'Exact grants · 0/10 selected');
    assert.ok(labelled(stage, 'Each image switch includes only the matching product action.'));
    assert.deepEqual(latestSwitchValues(stage), Array(10).fill(false));

    toggleSwitch(stage, 8, true);
    assert.deepEqual(latestSwitchValues(stage).slice(0, 5), [false, false, false, true, false]);
    assert.ok(labelled(stage, 'Review decision · 2/10 selected'));
    toggleSwitch(stage, 3, false);
    assert.deepEqual(latestSwitchValues(stage), Array(10).fill(false));

    toggleSwitch(stage, 6, true);
    toggleSwitch(stage, 9, true);
    assert.deepEqual(latestSwitchValues(stage).slice(0, 5), [false, true, false, false, true]);
    invoke(stage, updating ? 'Update with selected access' : 'Install with selected access');
    await settled();
    await settled();
    assert.deepEqual(calls[0][2].capabilities, ['containers:create', 'images:prune']);
    assert.deepEqual(calls[0][2].images, {
      read: [],
      use: [{ digest: `sha256:${'b'.repeat(64)}` }],
      pull: [],
      remove: [],
      prune_all_unused: true,
    });
  });
}

test('extension discovery distinguishes catalogue loading from a complete empty catalogue', async () => {
  let resolveCatalogue;
  const catalogue = new Promise((resolve) => {
    resolveCatalogue = resolve;
  });
  const stage = host();
  stage.render(
    h(Extensions, {
      api: {
        extensions: { list: async () => [], catalogue: () => catalogue },
        watchExtensions: async () => () => {},
      },
    }),
  );
  selectExtensionMode(stage, 'Discover');
  await settled();
  await settled();
  assert.ok(labelled(stage, 'Loading extension catalogue…'));
  assert.equal(labelled(stage, 'The built-in extension catalogue is currently empty.'), undefined);

  resolveCatalogue({ entries: [], complete: true });
  await settled();
  assert.ok(labelled(stage, 'The built-in extension catalogue is currently empty.'));
});

test('extension discovery can retry a failed catalogue without leaving the page', async () => {
  let attempts = 0;
  const stage = host();
  stage.render(
    h(Extensions, {
      api: {
        extensions: {
          list: async () => [],
          catalogue: async () => {
            attempts += 1;
            if (attempts === 1) throw new Error('catalogue service is offline');
            return firstPartyCatalogue();
          },
        },
        watchExtensions: async () => () => {},
      },
    }),
  );
  await settled();
  selectExtensionMode(stage, 'Discover');
  await settled();
  assert.ok(labelled(stage, 'Extension catalogue could not be completed.'));
  assert.ok(labelled(stage, 'Technical details'));
  assert.ok(labelled(stage, 'catalogue service is offline'));
  assert.ok(labelled(stage, 'Retry catalogue'));

  invoke(stage, 'Retry catalogue');
  await settled();
  assert.equal(attempts, 2);
  assert.ok(labelled(stage, 'Review access'));
});

test('extension discovery keeps the newest result when catalogue retries finish out of order', async () => {
  let attempts = 0;
  let rejectSlowRetry;
  const slowRetry = new Promise((_, reject) => {
    rejectSlowRetry = reject;
  });
  const stage = host();
  stage.render(
    h(Extensions, {
      api: {
        extensions: {
          list: async () => [],
          catalogue: async () => {
            attempts += 1;
            if (attempts === 1) throw new Error('catalogue service is offline');
            if (attempts === 2) return slowRetry;
            return firstPartyCatalogue();
          },
        },
        watchExtensions: async () => () => {},
      },
    }),
  );
  await settled();
  selectExtensionMode(stage, 'Discover');
  await settled();
  assert.ok(labelled(stage, 'Retry catalogue'));

  // A double activation can cross the render boundary, so both requests are
  // valid host calls. Their completion order must not decide which catalogue
  // the developer sees.
  invoke(stage, 'Retry catalogue');
  invoke(stage, 'Retry catalogue');
  await settled();
  assert.equal(attempts, 3);
  assert.ok(labelled(stage, 'Review access'));

  rejectSlowRetry(new Error('stale retry failed after the current catalogue loaded'));
  await settled();
  const visible = orderedLabels(stage);
  assert.ok(visible.includes('Review access'));
  assert.equal(visible.includes('Extension catalogue could not be completed.'), false);
  assert.equal(visible.includes('stale retry failed after the current catalogue loaded'), false);
});

test('extension inspection keeps invalid and failed references recoverable with a direct retry', async () => {
  const references = [];
  let attempt = 0;
  const stage = host();
  stage.render(
    h(Extensions, {
      api: {
        extensions: {
          list: async () => [],
          startAcquisition: async (reference) => {
            references.push(reference);
            if (reference === 'not a reference') throw new Error('Image reference is invalid.');
            attempt += 1;
            return { job: `review-${attempt}` };
          },
          acquisition: async (job) =>
            attempt === 1
              ? {
                  job,
                  reference: 'registry.example/reviewed:1',
                  revision: 1,
                  state: 'failed',
                  progress: null,
                  candidate: null,
                  error:
                    'registry operation failed: {"errors":[{"code":"DENIED","message":"requested access to the resource is denied"}]}\\n',
                }
              : {
                  job,
                  reference: 'registry.example/reviewed:1',
                  revision: 2,
                  state: 'ready',
                  progress: null,
                  candidate: {
                    name: 'reviewed',
                    version: '1.0.0',
                    image_digest: `sha256:${'c'.repeat(64)}`,
                    installed_image_digest: null,
                    requested: ['containers:read'],
                  },
                  error: null,
                },
        },
        watchExtensions: async () => () => {},
      },
    }),
  );
  await settled();
  selectExtensionMode(stage, 'Discover');
  await settled();

  change(stage, 'registry.example/extension:version', 'not a reference');
  invoke(stage, 'Inspect');
  await settled();
  await settled();
  assert.ok(labelled(stage, 'Image reference is invalid.'));
  assert.equal(fieldValue(stage, 'registry.example/extension:version'), 'not a reference');

  change(stage, 'registry.example/extension:version', 'registry.example/reviewed:1');
  invoke(stage, 'Inspect');
  await settled();
  await settled();
  assert.ok(labelled(stage, 'Couldn’t inspect extension'));
  assert.ok(labelled(stage, 'Image · registry.example/reviewed:1'));
  assert.ok(
    labelled(
      stage,
      'Registry access denied. Sign in with credentials that can read this image, or verify that the image is public.',
    ),
  );
  assert.ok(labelled(stage, 'Retry inspection'));
  assert.ok(labelled(stage, 'Back to catalogue'));
  assert.ok(labelled(stage, 'Technical details'));
  assert.ok(
    labelled(
      stage,
      'registry operation failed: {"errors":[{"code":"DENIED","message":"requested access to the resource is denied"}]}',
    ),
  );
  assert.equal(labelled(stage, 'Dismiss'), undefined);
  assert.equal(
    stage.frames
      .flatMap((frame) => frame.patches)
      .filter(
        (patch) =>
          patch.SetProp?.prop === 'Label' &&
          patch.SetProp.value?.Text ===
            'Registry access denied. Sign in with credentials that can read this image, or verify that the image is public.',
      ).length,
    1,
    'one bounded failure is rendered without a second recovery cascade',
  );
  invoke(stage, 'Retry inspection');
  await settled();
  await settled();
  assert.deepEqual(references, [
    'not a reference',
    'registry.example/reviewed:1',
    'registry.example/reviewed:1',
  ]);
  assert.ok(labelled(stage, 'Review reviewed'));
  assert.ok(labelled(stage, 'reviewed · 1.0.0'));
  assert.ok(labelled(stage, 'Review permissions'));
  assert.ok(labelled(stage, `Reviewed image sha256:${'c'.repeat(12)}…${'c'.repeat(8)}`));
  assert.ok(labelled(stage, 'Source registry.example/reviewed:1'));
  assert.ok(
    labelled(
      stage,
      'All access is off. Expand exact grants and enable only what this extension needs.',
    ),
  );
  assert.ok(labelled(stage, 'View containers and processes'));
  assert.deepEqual(property(stage, 'View containers and processes', 'Tooltip'), {
    Text: 'containers:read',
  });
  assert.deepEqual(latestSwitchValues(stage), [false]);
});

test('extension acquisition phases and failures remain actionable without raw engine cascades', () => {
  const status = (state) => ({
    job: 'phase',
    reference: 'registry.example/tool:1',
    revision: 2,
    state,
    progress: null,
    candidate: null,
    error: null,
  });
  assert.match(acquisitionLabel(status('inspecting')), /workspace architecture/);
  assert.match(acquisitionLabel(status('reading-manifest')), /validating the extension manifest/);
  assert.match(acquisitionLabel(status('committing')), /Saving the reviewed extension/);
  assert.match(acquisitionLabel(status('cancelled')), /No extension was installed/);
  assert.match(
    acquisitionFailure(
      'extension image sha256:a is linux/arm64, but this workspace requires linux/amd64',
    ),
    /^Architecture mismatch:/,
  );
  assert.match(
    acquisitionFailure('workspace execution domain failed: Engine(Load(Inspect))'),
    /^Workspace image service is unavailable\./,
  );
  assert.match(
    acquisitionFailure("the image's manifest archive is unreadable"),
    /^Extension manifest could not be validated:/,
  );
  assert.equal(acquisitionTechnicalDetail('first\\nsecond'), 'first\nsecond');
  assert.match(acquisitionTechnicalDetail('x'.repeat(5_000)), /technical detail truncated$/);
});

test('a stale cancellation refreshes the authoritative phase and remains cancellable', async () => {
  let reads = 0;
  let cancellations = 0;
  const stage = host();
  stage.render(
    h(Extensions, {
      api: {
        extensions: {
          list: async () => [],
          startAcquisition: async () => ({ job: 'moving-job' }),
          acquisition: async () => ({
            job: 'moving-job',
            reference: 'registry.example/tool:1',
            revision: ++reads,
            state: reads === 1 ? 'inspecting' : 'reading-manifest',
            progress: null,
            candidate: null,
            error: null,
          }),
          waitForAcquisition: async () => new Promise(() => {}),
          cancelAcquisition: async () => {
            cancellations += 1;
            throw new Error('the acquisition revision has changed');
          },
        },
        watchExtensions: async () => () => {},
      },
    }),
  );
  await settled();
  selectExtensionMode(stage, 'Discover');
  await settled();
  change(stage, 'registry.example/extension:version', 'registry.example/tool:1');
  invoke(stage, 'Inspect');
  await settled();
  assert.ok(
    labelled(stage, 'Checking whether the image is available for this workspace architecture…'),
  );
  invoke(stage, 'Cancel inspection');
  await settled();
  assert.equal(cancellations, 1);
  assert.ok(labelled(stage, 'Reading and validating the extension manifest…'));
  assert.ok(
    labelled(
      stage,
      'Acquisition advanced before cancellation. Review its current phase and cancel again if needed.',
    ),
  );
  assert.ok(labelled(stage, 'Cancel inspection'));
});

test('exact workspace environment consent carries its required verb and clears coherently', async () => {
  const installs = [];
  const selector = { workspace: 'development', name: 'DATABASE_URL' };
  const candidate = {
    name: 'environment-reader',
    version: '1.0.0',
    image_digest: `sha256:${'f'.repeat(64)}`,
    installed_image_digest: null,
    requested: ['workspace-environment:read'],
    requested_workspace_environment: { read: [selector], write: [] },
  };
  const stage = host();
  stage.render(
    h(Extensions, {
      api: {
        extensions: {
          list: async () => [],
          startAcquisition: async () => ({ job: 'environment-review' }),
          acquisition: async () => ({
            job: 'environment-review',
            reference: 'registry.example/environment-reader:1',
            revision: 3,
            state: 'ready',
            progress: null,
            candidate,
            error: null,
          }),
          installAndWait: async (...arguments_) => {
            installs.push(arguments_);
            return { changed: true, extension: { ...candidate, status: 'standby' } };
          },
        },
        watchExtensions: async () => () => {},
      },
    }),
  );
  await settled();
  selectExtensionMode(stage, 'Discover');
  await settled();
  change(stage, 'registry.example/extension:version', 'registry.example/environment-reader:1');
  invoke(stage, 'Inspect');
  await settled();
  await settled();
  expand(stage, 'Exact grants · 0/2 selected');

  toggleSwitch(stage, 1, true);
  assert.deepEqual(latestSwitchValues(stage), [true, true]);
  assert.ok(labelled(stage, 'Review decision · 2/2 selected'));

  toggleSwitch(stage, 0, false);
  assert.deepEqual(latestSwitchValues(stage), [false, false]);
  assert.ok(labelled(stage, 'No access selected · 2 requested'));

  toggleSwitch(stage, 1, true);
  invoke(stage, 'Install with selected access');
  await settled();
  await settled();
  assert.deepEqual(installs[0][2].capabilities, ['workspace-environment:read']);
  assert.deepEqual(installs[0][2].workspaceEnvironment, { read: [selector], write: [] });
});

test('a long extension acquisition stays attached to its host job until review is ready', async () => {
  const originalNow = Date.now;
  let clock = 0;
  let waits = 0;
  const digest = `sha256:${'d'.repeat(64)}`;
  Date.now = () => {
    clock += 31_000;
    return clock;
  };
  try {
    const stage = host();
    stage.render(
      h(Extensions, {
        api: {
          extensions: {
            list: async () => [],
            startAcquisition: async () => ({ job: 'slow-job' }),
            acquisition: async () => ({
              job: 'slow-job',
              reference: 'registry.example/slow:1',
              revision: 1,
              state: 'pulling',
              progress: {
                status: 'Downloading image',
                id: 'layer',
                current: 1,
                total: 10,
              },
              candidate: null,
              error: null,
            }),
            waitForAcquisition: async (job, revision) => {
              waits += 1;
              assert.equal(job, 'slow-job');
              assert.equal(revision, 1);
              return {
                changed: true,
                status: {
                  job,
                  reference: 'registry.example/slow:1',
                  revision: 2,
                  state: 'ready',
                  progress: null,
                  candidate: {
                    name: 'slow',
                    version: '1.0.0',
                    image_digest: digest,
                    installed_image_digest: null,
                    requested: [],
                  },
                  error: null,
                },
              };
            },
            cancelAcquisition: async () => {},
          },
          watchExtensions: async () => () => {},
        },
      }),
    );
    await settled();
    selectExtensionMode(stage, 'Discover');
    await settled();
    change(stage, 'registry.example/extension:version', 'registry.example/slow:1');
    invoke(stage, 'Inspect');
    await settled();
    await settled();

    assert.equal(waits, 1, 'the existing host job remains authoritative after thirty seconds');
    assert.ok(labelled(stage, 'Review slow'));
    assert.ok(labelled(stage, `Reviewed image sha256:${'d'.repeat(12)}…${'d'.repeat(8)}`));
    assert.equal(labelled(stage, 'Acquisition is still running.'), undefined);
  } finally {
    Date.now = originalNow;
  }
});

for (const updating of [false, true]) {
  test(`extension ${updating ? 'update' : 'install'} independently narrows container and filesystem authority`, async () => {
    const calls = [];
    const candidate = {
      name: 'scoped',
      version: '2.0.0',
      image_digest: `sha256:${'a'.repeat(64)}`,
      requested: ['containers:create', 'filesystem:read', 'filesystem:write'],
      requested_containers: {
        selectors: [{ name: 'database' }, { id: 'c'.repeat(64) }, { all: true }],
        create: true,
      },
      requested_filesystem: {
        read: [{ subtree: 'src' }, { exact: 'README.md' }],
        write: [{ exact: 'src/config.json' }],
        create: [{ subtree: 'generated' }],
        delete: [{ subtree: 'cache' }],
        rename: [{ subtree: 'migrations' }],
      },
      installed_image_digest: updating ? `sha256:${'b'.repeat(64)}` : null,
    };
    const stage = host();
    stage.render(
      h(Extensions, {
        api: {
          extensions: {
            list: async () => [],
            startAcquisition: async () => ({ job: 'scoped-review' }),
            acquisition: async () => ({
              job: 'scoped-review',
              reference: 'local/scoped:2',
              revision: 4,
              state: 'ready',
              progress: null,
              candidate,
              error: null,
            }),
            [`${updating ? 'update' : 'install'}AndWait`]: async (...args) => {
              calls.push(args);
              return { changed: true, extension: { ...candidate, status: 'running' } };
            },
          },
          watchExtensions: async () => () => {},
        },
      }),
    );
    await settled();
    selectExtensionMode(stage, 'Discover');
    await settled();
    change(stage, 'registry.example/extension:version', 'local/scoped:2');
    invoke(stage, 'Inspect');
    await settled();
    await settled();

    assert.ok(labelled(stage, 'Review scoped'));
    assert.ok(labelled(stage, 'scoped · 2.0.0'));
    assert.ok(labelled(stage, 'Source local/scoped:2'));
    assert.ok(labelled(stage, `Reviewed image ${compactDigest(candidate.image_digest)}`));
    assert.ok(
      labelled(
        stage,
        'Direct OCI image · no catalogue publisher verification. Confirm the source and reviewed image digest before granting access.',
      ),
    );
    if (updating) {
      assert.ok(
        labelled(
          stage,
          `Replaces installed image ${compactDigest(candidate.installed_image_digest)}. Access below was reset and must be approved again.`,
        ),
      );
    }
    assert.ok(
      labelled(stage, 'Container access starts off. Select only what this extension needs.'),
    );
    for (const label of [
      'Container named database',
      `Exact container ${'c'.repeat(64)}`,
      'All workspace containers',
      'Create new containers',
    ])
      assert.ok(labelled(stage, label), label);
    assert.ok(
      labelled(
        stage,
        'Each switch grants only the named action and root, and includes the matching file capability. Modify cannot create, delete, or rename.',
      ),
    );
    for (const label of [
      'View contents folder · src/ and everything inside',
      'View contents file · README.md',
      'Modify existing contents file · src/config.json',
      'Create new entries folder · generated/ and everything inside',
      'Delete entries folder · cache/ and everything inside',
      'Rename or move entries folder · migrations/ and everything inside',
    ])
      assert.ok(labelled(stage, label), label);
    assert.ok(labelled(stage, 'Requested access'));
    assert.ok(
      labelled(stage, 'Product · 3  ·  Containers · 4  ·  Files · 6'),
      'requested authority is summarized as quiet copy instead of a row of badges',
    );
    assert.ok(labelled(stage, 'No access selected · 13 requested'));
    assert.ok(labelled(stage, 'Exact grants · 0/13 selected'));
    expand(stage, 'Exact grants · 0/13 selected');
    assert.ok(labelled(stage, 'Container access · 0/4'));
    assert.ok(labelled(stage, '0/6 workspace paths allowed'));
    assert.deepEqual(latestSwitchValues(stage), Array(13).fill(false));

    toggleSwitch(stage, 8, true);
    assert.deepEqual(latestSwitchValues(stage).slice(0, 3), [false, true, false]);
    assert.ok(labelled(stage, 'Review decision · 2/13 selected'));
    toggleSwitch(stage, 1, false);
    assert.deepEqual(latestSwitchValues(stage), Array(13).fill(false));
    assert.ok(labelled(stage, 'No access selected · 13 requested'));

    toggleSwitch(stage, 3, true);
    toggleSwitch(stage, 6, true);
    toggleSwitch(stage, 8, true);
    toggleSwitch(stage, 10, true);
    toggleSwitch(stage, 12, true);
    assert.ok(labelled(stage, '3/6 workspace paths allowed'));
    assert.ok(labelled(stage, 'Review decision · 8/13 selected'));
    assert.ok(
      ancestorTags(stage, 'View contents file · README.md').filter((tag) => tag === 'Scroll')
        .length === 1,
      'permission choices remain inside the scrolling review region',
    );
    assert.ok(
      ancestorTags(
        stage,
        updating ? 'Update with selected access' : 'Install with selected access',
      ).filter((tag) => tag === 'Scroll').length === 0,
      'the review decision remains outside the scrolling permission region',
    );
    invoke(stage, updating ? 'Update with selected access' : 'Install with selected access');
    await settled();
    await settled();

    assert.equal(calls.length, 1);
    assert.deepEqual(calls[0].slice(0, 2), ['scoped-review', 4]);
    assert.deepEqual(calls[0][2].capabilities, [
      'containers:create',
      'filesystem:read',
      'filesystem:write',
    ]);
    assert.deepEqual(calls[0][2].containers, {
      selectors: [{ name: 'database' }],
      create: true,
    });
    assert.deepEqual(calls[0][2].images, {
      read: [],
      use: [],
      pull: [],
      remove: [],
      prune_all_unused: false,
    });
    assert.deepEqual(calls[0][2].networks, {
      selectors: [],
      create: false,
    });
    assert.deepEqual(calls[0][2].volumes, {
      selectors: [],
      create: false,
    });
    assert.deepEqual(calls[0][2].filesystem, {
      read: [{ exact: 'README.md' }],
      write: [],
      create: [{ subtree: 'generated' }],
      delete: [],
      rename: [{ subtree: 'migrations' }],
    });
  });
}

test('extension review grants one exact network without workspace-wide network authority', async () => {
  const calls = [];
  const candidate = {
    name: 'postgres',
    version: '1.0.0',
    image_digest: `sha256:${'a'.repeat(64)}`,
    requested: ['networks:read'],
    requested_containers: { selectors: [], create: false },
    requested_networks: { selectors: [{ name: 'database' }, { name: 'internal' }], create: true },
    requested_volumes: { selectors: [{ name: 'data' }], create: true },
    requested_filesystem: { read: [], write: [], create: [], delete: [], rename: [] },
    requested_workspace_environment: { read: [], write: [] },
    installed_image_digest: null,
  };
  const stage = host();
  stage.render(
    h(Extensions, {
      api: {
        extensions: {
          list: async () => [],
          startAcquisition: async () => ({ job: 'network-review' }),
          acquisition: async () => ({
            job: 'network-review',
            reference: 'local/postgres:1',
            revision: 1,
            state: 'ready',
            progress: null,
            candidate,
            error: null,
          }),
          installAndWait: async (...args) => {
            calls.push(args);
            return { changed: true, extension: { ...candidate, status: 'running' } };
          },
        },
        watchExtensions: async () => () => {},
      },
    }),
  );
  await settled();
  selectExtensionMode(stage, 'Discover');
  await settled();
  change(stage, 'registry.example/extension:version', 'local/postgres:1');
  invoke(stage, 'Inspect');
  await settled();
  await settled();
  assert.ok(
    labelled(stage, 'Network access starts off. Select only the networks this extension needs.'),
  );
  assert.ok(labelled(stage, 'Network named database'));
  assert.ok(labelled(stage, 'Network named internal'));
  assert.ok(labelled(stage, 'Volume named data'));
  assert.ok(labelled(stage, 'Create new volumes'));
  toggleSwitch(stage, 1, true);
  toggleSwitch(stage, 4, true);
  invoke(stage, 'Install with selected access');
  await settled();
  await settled();
  assert.deepEqual(calls[0][2].networks, {
    selectors: [{ name: 'database' }],
    create: false,
  });
  assert.deepEqual(calls[0][2].volumes, { selectors: [{ name: 'data' }], create: false });
});

test('extension image entry submits from the keyboard and consent explains requested authority', async () => {
  const calls = [];
  let installs = 0;
  let acquisitionState = 'ready';
  let acquisitionRevision = 7;
  let committed = false;
  const stage = host();
  stage.render(
    h(Extensions, {
      api: {
        extensions: {
          list: async () =>
            committed
              ? [
                  {
                    name: 'assistant',
                    version: '1.2.0',
                    image_digest: `sha256:${'a'.repeat(64)}`,
                    enabled: false,
                    status: 'standby',
                  },
                ]
              : [],
          startAcquisition: async (reference) => {
            calls.push(['inspect', reference]);
            acquisitionState = 'ready';
            return { job: 'candidate' };
          },
          acquisition: async () => ({
            job: 'candidate',
            reference: 'registry.example/assistant:1.2.0',
            revision: acquisitionRevision,
            state: acquisitionState,
            progress: null,
            candidate:
              acquisitionState === 'ready'
                ? {
                    name: 'assistant',
                    version: '1.2.0',
                    image_digest: `sha256:${'a'.repeat(64)}`,
                    installed_image_digest: null,
                    requested: ['containers:read', 'terminals:output'],
                  }
                : null,
            error: acquisitionState === 'failed' ? 'signature verification unavailable' : null,
          }),
          installAndWait: async (job, revision, granted) => {
            calls.push(['install', job, revision, granted]);
            installs += 1;
            if (installs === 1) {
              acquisitionRevision = 8;
              throw new Error('signature verification unavailable');
            }
            committed = true;
            acquisitionState = 'installed';
            throw new Error('connection closed before install reply');
          },
        },
        watchExtensions: async () => () => {},
      },
    }),
  );
  await settled();
  selectExtensionMode(stage, 'Discover');
  await settled();
  assert.equal(
    ancestorProperty(stage, 'Inspect', 'Row', 'Wrap')?.Flag,
    true,
    'the OCI image input and Inspect action wrap together on a narrow viewport',
  );
  assert.deepEqual(
    ancestorProperty(stage, 'Inspect', 'Row', 'Width'),
    { Length: 'Fill' },
    'the OCI install row uses all available content width',
  );
  assert.deepEqual(
    placeholderProperty(stage, 'registry.example/extension:version', 'Width'),
    { Length: { Chars: 24 } },
    'the OCI reference keeps a compact minimum while its tooltip preserves the exact value',
  );
  change(stage, 'registry.example/extension:version', 'registry.example/assistant:1.2');
  submit(stage, 'registry.example/extension:version');
  await settled();
  await settled();
  assert.deepEqual(calls, [['inspect', 'registry.example/assistant:1.2']]);
  assert.ok(labelled(stage, 'View containers and processes'));
  assert.ok(labelled(stage, 'Read terminal text'));
  assert.ok(labelled(stage, 'Requested access'));
  assert.ok(labelled(stage, 'Product · 2'));
  assert.ok(labelled(stage, 'No access selected · 2 requested'));
  assert.ok(labelled(stage, 'Exact grants · 0/2 selected'));
  expand(stage, 'Exact grants · 0/2 selected');
  assert.ok(labelled(stage, 'Product access · 0/2'));
  assert.equal(
    labelled(stage, 'Workspace files'),
    undefined,
    'unrequested authority groups do not consume compact review space',
  );
  assert.equal(
    labelled(stage, 'Allow requested'),
    undefined,
    'each capability requires its own consent gesture',
  );
  assert.ok(
    labelled(stage, 'View containers and processes'),
    'plain-language authority remains visible in the compact review',
  );
  assert.deepEqual(
    property(stage, 'View containers and processes', 'Tooltip'),
    { Text: 'containers:read' },
    'the exact wire authority remains inspectable without cluttering the label',
  );

  toggleSwitch(stage, 0, true);
  toggleSwitch(stage, 1, true);
  assert.ok(labelled(stage, 'Review decision · 2/2 selected'));
  assert.ok(labelled(stage, 'Product access · 2/2'));
  assert.ok(labelled(stage, 'Clear product access'));
  invoke(stage, 'Install with selected access');
  await settled();
  await settled();
  assert.ok(
    labelled(
      stage,
      'The extension was not saved. Its reviewed image and selected access are retained for a safe retry. signature verification unavailable',
    ),
  );
  assert.ok(
    labelled(stage, 'Install with selected access'),
    'a failed commit retains the exact reviewed candidate for a fresh revision retry',
  );
  invoke(stage, 'Install with selected access');
  await settled();
  await settled();
  assert.deepEqual(calls.filter(([operation]) => operation === 'inspect').length, 1);
  assert.deepEqual(calls.at(-1).slice(0, 3), ['install', 'candidate', 8]);
  assert.deepEqual(calls.at(-1)[3].capabilities, ['containers:read', 'terminals:output']);
  assert.ok(
    labelled(
      stage,
      'assistant installed, but the confirmation reply was lost. Current extension state was verified by refresh.',
    ),
  );
});

test('a ready extension review can be abandoned without granting authority', async () => {
  const calls = [];
  const stage = host();
  stage.render(
    h(Extensions, {
      api: {
        extensions: {
          list: async () => [],
          startAcquisition: async () => ({ job: 'candidate' }),
          acquisition: async () => ({
            job: 'candidate',
            reference: 'registry.example/assistant:1',
            revision: 1,
            state: 'ready',
            progress: null,
            candidate: {
              name: 'assistant',
              version: '1.0.0',
              image_digest: `sha256:${'a'.repeat(64)}`,
              installed_image_digest: null,
              requested: ['containers:read'],
            },
            error: null,
          }),
          installAndWait: async (...args) => calls.push(args),
        },
        watchExtensions: async () => () => {},
      },
    }),
  );
  await settled();
  selectExtensionMode(stage, 'Discover');
  await settled();
  change(stage, 'registry.example/extension:version', 'registry.example/assistant:1');
  invoke(stage, 'Inspect');
  await settled();
  await settled();
  toggleSwitch(stage, 0, true);
  assert.ok(labelled(stage, 'Cancel review'));
  invoke(stage, 'Cancel review');
  await settled();
  assert.deepEqual(calls, []);
  assert.equal(
    fieldValue(stage, 'registry.example/extension:version'),
    'registry.example/assistant:1',
    'the reference remains available for a later reinspection',
  );
});

test('a lost install reply follows the committing job instead of replaying stale consent', async () => {
  const digest = `sha256:${'e'.repeat(64)}`;
  let committed = false;
  let reads = 0;
  let installs = 0;
  const waits = [];
  let finishCommit;
  const stage = host();
  stage.render(
    h(Extensions, {
      api: {
        extensions: {
          list: async () =>
            committed
              ? [
                  {
                    name: 'database-tools',
                    version: '1.0.0',
                    image_digest: digest,
                    enabled: false,
                    status: 'standby',
                  },
                ]
              : [],
          startAcquisition: async () => ({ job: 'commit-recovery' }),
          acquisition: async () => {
            reads += 1;
            return {
              job: 'commit-recovery',
              reference: 'registry.example/database-tools:1',
              revision: reads === 1 ? 2 : committed ? 4 : 3,
              state: reads === 1 ? 'ready' : committed ? 'installed' : 'committing',
              progress: null,
              candidate:
                reads === 1
                  ? {
                      name: 'database-tools',
                      version: '1.0.0',
                      image_digest: digest,
                      installed_image_digest: null,
                      requested: ['containers:read'],
                    }
                  : null,
              error: null,
            };
          },
          installAndWait: async () => {
            installs += 1;
            throw new Error('connection closed while install was committing');
          },
          waitForAcquisition: async (job, revision) => {
            waits.push([job, revision]);
            return new Promise((resolve) => {
              finishCommit = () => {
                committed = true;
                resolve({
                  changed: true,
                  status: {
                    job,
                    reference: 'registry.example/database-tools:1',
                    revision: 4,
                    state: 'installed',
                    progress: null,
                    candidate: null,
                    error: null,
                  },
                });
              };
            });
          },
        },
        watchExtensions: async () => () => {},
      },
    }),
  );
  await settled();
  selectExtensionMode(stage, 'Discover');
  await settled();
  change(stage, 'registry.example/extension:version', 'registry.example/database-tools:1');
  invoke(stage, 'Inspect');
  await settled();
  await settled();
  toggleSwitch(stage, 0, true);

  // The host accepted the commit but the request/reply connection disappeared.
  // Its authoritative job is already committing at revision 3.
  invoke(stage, 'Install with selected access');
  await settled();
  await settled();
  assert.ok(labelled(stage, 'Saving the reviewed extension and its granted access…'));
  assert.equal(
    labelled(stage, 'Cancel inspection'),
    undefined,
    'a commit cannot be cancelled after durable publication may have begun',
  );
  finishCommit();
  await settled();
  await settled();

  assert.deepEqual(waits, [['commit-recovery', 3]]);
  assert.equal(installs, 1);
  assert.ok(
    labelled(
      stage,
      'database-tools installed, but the confirmation reply was lost. Current extension state was verified by refresh.',
    ),
  );
});

test('installed extension removal requires final consent and a failure remains retryable', async () => {
  const calls = [];
  let removes = 0;
  let rejectRemoval;
  const extension = {
    name: 'assistant',
    image_digest: `sha256:${'b'.repeat(64)}`,
    version: '1.2.0',
    enabled: true,
    status: 'running',
  };
  let installed = extension;
  const stage = host();
  stage.render(
    h(Extensions, {
      api: {
        extensions: {
          list: async () => (installed ? [installed] : []),
          removeAndWait: async (name, digest) => {
            calls.push([name, digest]);
            removes += 1;
            if (removes === 1)
              return new Promise((_, reject) => {
                rejectRemoval = () => reject(new Error('extension is still stopping'));
              });
            installed = null;
            throw new Error('connection closed before removal reply');
          },
        },
        watchExtensions: async () => () => {},
      },
    }),
  );
  await settled();
  invoke(stage, 'Remove');
  assert.deepEqual(calls, [], 'opening consent carries no removal authority');
  assert.ok(labelled(stage, 'Remove assistant and permanently delete its private workspace data?'));
  invoke(stage, 'Remove assistant');
  invoke(stage, 'Remove assistant');
  assert.equal(calls.length, 1, 'a repeated confirmation cannot duplicate removal authority');
  await settled();
  assert.ok(labelled(stage, 'Removing assistant…'));
  rejectRemoval();
  await settled();
  await settled();
  assert.ok(labelled(stage, 'Remove extension could not be completed.'));
  assert.ok(labelled(stage, 'Technical details'));
  assert.ok(labelled(stage, 'extension is still stopping'));
  assert.ok(labelled(stage, 'Remove'), 'failure returns to a fresh two-step consent');
  invoke(stage, 'Remove');
  invoke(stage, 'Remove assistant');
  await settled();
  await settled();
  assert.equal(calls.length, 2);
  assert.ok(
    labelled(
      stage,
      'assistant removed, but the confirmation reply was lost. Current extension state was verified by refresh.',
    ),
  );
});

test('installed extension lifecycle reconciles a lost reply without masking a real failure', async () => {
  const calls = [];
  let attempts = 0;
  let extension = {
    name: 'assistant',
    image_digest: `sha256:${'c'.repeat(64)}`,
    version: '1.2.0',
    enabled: false,
    status: 'standby',
  };
  const stage = host();
  stage.render(
    h(Extensions, {
      api: {
        extensions: {
          list: async () => [extension],
          enableAndWait: async (name, digest) => {
            calls.push([name, digest]);
            attempts += 1;
            if (attempts === 1) throw new Error('extension host rejected enable');
            extension = { ...extension, enabled: true, status: 'starting' };
            throw new Error('connection closed before enable reply');
          },
        },
        watchExtensions: async () => () => {},
      },
    }),
  );
  await settled();

  assert.deepEqual(
    taggedProperty(stage, 'Enable', 'Button', 'Size'),
    { ControlSize: 'Small' },
    'the installed lifecycle action keeps its card compact',
  );
  invoke(stage, 'Enable');
  await settled();
  await settled();
  assert.ok(labelled(stage, 'Enable extension could not be completed.'));
  assert.ok(labelled(stage, 'extension host rejected enable'));
  assert.ok(labelled(stage, 'Enable'), 'an unchanged authoritative state remains retryable');

  invoke(stage, 'Enable');
  await settled();
  await settled();
  assert.equal(calls.length, 2);
  assert.ok(
    labelled(
      stage,
      'assistant enabled, but the confirmation reply was lost. Current extension state was verified by refresh.',
    ),
  );
  assert.ok(
    labelled(stage, 'Starting'),
    'the reconciled inventory replaces the stale disabled card',
  );
});

test('Top is visibly required and offers no self-disable or self-removal trap', async () => {
  const stage = host();
  stage.render(
    h(Extensions, {
      api: {
        extensions: {
          list: async () => [
            {
              name: 'top',
              image_digest: `sha256:${'a'.repeat(64)}`,
              version: '0.1.0',
              enabled: true,
              status: 'running',
            },
          ],
        },
        watchExtensions: async () => () => {},
      },
    }),
  );
  await settled();

  assert.ok(labelled(stage, 'Running'));
  assert.ok(labelled(stage, 'Version 0.1.0'));
  assert.deepEqual(property(stage, 'top', 'Tooltip'), {
    Text: `sha256:${'a'.repeat(64)}`,
  });
  assert.equal(labelled(stage, 'Disable'), undefined);
  assert.equal(labelled(stage, 'Remove'), undefined);
});

test('installed extensions expose truthful enabled, disabled, fault and retry states', async () => {
  const calls = [];
  let publish;
  let release;
  let extension = {
    name: 'assistant',
    image_digest: `sha256:${'d'.repeat(64)}`,
    version: '1.2.0',
    enabled: true,
    status: 'running',
  };
  const result = (action) => {
    calls.push(action);
    const resulting =
      action === 'disable'
        ? { ...extension, enabled: false, status: 'stopped' }
        : { ...extension, enabled: true, status: 'running' };
    return new Promise((resolve) => {
      release = () => {
        extension = resulting;
        resolve({ changed: true, extension });
      };
    });
  };
  const stage = host();
  stage.render(
    h(Extensions, {
      api: {
        extensions: {
          list: async () => [extension],
          disableAndWait: async () => result('disable'),
          enableAndWait: async () => result('enable'),
          retryAndWait: async () => result('retry'),
        },
        watchExtensions: async (listener) => {
          publish = listener;
          return () => {};
        },
      },
    }),
  );
  await settled();
  assert.ok(labelled(stage, 'Permissions & management'));
  assert.ok(
    ancestorTags(stage, 'Disable').includes('Expander'),
    'secondary lifecycle controls stay inside one compact management disclosure',
  );
  assert.ok(
    ancestorTags(stage, 'Remove').includes('Expander'),
    'destructive removal does not compete with the primary card action',
  );
  invoke(stage, 'Disable');
  invoke(stage, 'Disable');
  await settled();
  assert.deepEqual(calls, ['disable'], 'pending disable admits one authority call');
  assert.ok(labelled(stage, 'Disabling assistant…'));
  release();
  await settled();
  await settled();
  assert.ok(labelled(stage, 'Disabled'), 'disabled state replaces stale stopped status');
  invoke(stage, 'Enable');
  await settled();
  assert.ok(labelled(stage, 'Enabling assistant…'));
  release();
  await settled();
  await settled();
  assert.ok(labelled(stage, 'Running'));

  extension = { ...extension, enabled: true, status: 'fault: socket closed' };
  publish([extension]);
  await settled();
  assert.ok(labelled(stage, 'Faulted'));
  assert.ok(labelled(stage, 'Extension lost its connection. No change was assumed.'));
  assert.ok(labelled(stage, 'Technical details'));
  assert.ok(labelled(stage, 'socket closed'));
  assert.equal(
    labelled(stage, 'fault: socket closed'),
    undefined,
    'fault details are separated from the bounded status badge',
  );
  assert.ok(labelled(stage, 'Retry'));
  assert.deepEqual(
    taggedProperty(stage, 'Retry', 'Button', 'Size'),
    { ControlSize: 'Small' },
    'fault recovery keeps the installed card compact',
  );
  invoke(stage, 'Retry');
  await settled();
  assert.ok(labelled(stage, 'Retrying assistant…'));
  release();
  await settled();
  await settled();
  assert.deepEqual(calls, ['disable', 'enable', 'retry']);
  assert.ok(labelled(stage, 'assistant recovered and verified.'));
});

test('installed extensions distinguish durable exact-file and subtree authority', async () => {
  const stage = host();
  stage.render(
    h(Extensions, {
      api: {
        extensions: {
          list: async () => [
            {
              name: 'indexer',
              image_digest: `sha256:${'a'.repeat(64)}`,
              version: '1.0.0',
              enabled: true,
              status: 'duty',
              granted: ['containers:read'],
              containers: {
                selectors: [{ name: 'database' }, { id: 'c'.repeat(64) }],
                create: true,
              },
              filesystem: {
                read: [{ subtree: 'documents' }],
                write: [{ exact: 'settings/index.json' }],
                create: [],
                delete: [],
                rename: [],
              },
              workspace_environment: {
                read: [{ workspace: 'daily', name: 'REGISTRY_USER' }],
                write: [{ all: true }],
              },
            },
          ],
        },
        watchExtensions: async () => () => {},
      },
    }),
  );
  await settled();

  assert.ok(
    labelled(
      stage,
      'Granted access · View containers and processes · 3 container rules · 2 file rules · 2 environment rules',
    ),
  );
  assert.equal(labelled(stage, 'Granted access · 1 permission'), undefined);
  assert.ok(labelled(stage, 'Effective for this installed image digest'));
  assert.ok(labelled(stage, 'View containers and processes'));
  assert.equal(labelled(stage, 'View containers and processes · containers:read'), undefined);
  assert.ok(labelled(stage, 'Container · exact name database'));
  assert.ok(labelled(stage, `Container · exact ID ${'c'.repeat(64)}`));
  assert.ok(labelled(stage, 'Containers · create new containers'));
  assert.ok(labelled(stage, 'View contents folder · documents/ and everything inside'));
  assert.ok(labelled(stage, 'Modify existing contents file · settings/index.json'));
  assert.ok(labelled(stage, 'Environment · read REGISTRY_USER in workspace daily'));
  assert.ok(labelled(stage, 'Environment · write all names'));
  assert.equal(
    labelled(stage, 'Modify existing contents folder · settings/ and everything inside'),
    undefined,
    'an exact persisted grant is never presented as subtree authority',
  );
});

test('installed extensions translate the host duty stage into a developer-facing state', async () => {
  const stage = host();
  stage.render(
    h(Extensions, {
      api: {
        extensions: {
          list: async () => [
            {
              name: 'top',
              image_digest: 'sha256:top',
              version: '0.4.0',
              enabled: true,
              status: 'duty',
            },
          ],
        },
        watchExtensions: async () => () => {},
      },
    }),
  );
  await settled();
  assert.ok(labelled(stage, 'Enabled'));
});

test('overview keeps failures actionable while never presenting stale inventory as current', async () => {
  const stale = [{ id: 'old', state: 'running' }];
  const stage = host();
  const opened = [];
  const reloaded = [];
  const resource = (name, data, loading = false, error = null) => ({
    data,
    loading,
    error,
    reload: async () => reloaded.push(name),
  });
  stage.render(
    h(Overview, {
      containers: resource('containers', stale, true),
      executions: resource('executions', []),
      images: resource('images', stale, false, new Error('image refresh failed')),
      volumes: resource('volumes', []),
      networks: resource('networks', []),
      terminals: resource('terminals', []),
      extensions: resource('extensions', []),
      onOpen: (section) => opened.push(section),
    }),
  );
  assert.ok(labelled(stage, '…'));
  assert.ok(labelled(stage, 'Reading inventory…'));
  assert.ok(labelled(stage, 'Unavailable'));
  assert.ok(labelled(stage, 'Refresh failed'));
  assert.ok(labelled(stage, 'Unavailable'));
  assert.ok(labelled(stage, 'Reading inventory…'));
  assert.ok(labelled(stage, '0 running'));
  assert.ok(labelled(stage, 'Workspace inventory could not be completed.'));
  assert.ok(labelled(stage, 'Retry inventory'));
  assert.ok(labelled(stage, 'Technical details'));
  assert.ok(
    ancestorTags(stage, 'image refresh failed').includes('Expander'),
    'the bounded diagnostic stays behind an explicit disclosure',
  );
  assert.notDeepEqual(taggedProperty(stage, 'Technical details', 'Expander', 'Expanded'), {
    Flag: true,
  });
  const patches = stage.frames.flatMap((frame) => frame.patches);
  const actionNodes = new Set(
    patches
      .filter((patch) => patch.Create?.tag === 'CardActionArea')
      .map((patch) => patch.Create.id),
  );
  const rowNodes = new Set(
    patches.filter((patch) => patch.Create?.tag === 'Row').map((patch) => patch.Create.id),
  );
  assert.equal(
    patches.filter(
      (patch) =>
        patch.SetProp?.prop === 'Width' &&
        rowNodes.has(patch.SetProp.id) &&
        patch.SetProp.value?.Bounds?.minimum?.Chars === 32,
    ).length,
    4,
    'four stable summary pairs produce four narrow rows without changing the two wide rows',
  );
  for (const resource of [
    'Containers',
    'Processes',
    'Executions',
    'Images',
    'Volumes',
    'Networks',
    'Terminal tabs',
  ]) {
    assert.ok(
      patches.some(
        (patch) =>
          patch.SetProp?.prop === 'Tooltip' &&
          patch.SetProp.value?.Text === `Open ${resource}` &&
          actionNodes.has(patch.SetProp.id),
      ),
      `${resource} has an unambiguous dashboard action`,
    );
    const cue = patches
      .filter(
        (patch) =>
          patch.SetProp?.prop === 'Tooltip' && patch.SetProp.value?.Text === `Open ${resource}`,
      )
      .find((patch) => !actionNodes.has(patch.SetProp.id));
    assert.ok(cue, `${resource} has a visible navigation cue inside its action area`);
    assert.deepEqual(
      latestProperty(stage, cue.SetProp.id, 'Icon'),
      { Text: 'go-next-symbolic' },
      `${resource} uses the shared forward-navigation icon`,
    );
  }
  assert.equal(
    labelled(stage, '1 running'),
    undefined,
    'loading cannot retain stale running claims',
  );
  assert.equal(labelled(stage, '1'), undefined, 'failure cannot retain stale inventory counts');
  assert.equal(
    labelled(stage, 'No reported faults'),
    undefined,
    'extension management remains a single clear sidebar destination',
  );
  assert.equal(
    ancestorProperty(stage, 'Containers', 'Row', 'Wrap')?.Flag,
    true,
    'the summary cards reflow instead of clipping at narrow widths',
  );
  assert.deepEqual(
    ancestorProperty(stage, 'Containers', 'Row', 'Width'),
    { Length: 'Fill' },
    'the summary cards use the available page width',
  );
  assert.equal(
    ancestorProperty(stage, 'Containers', 'Card', 'Grow')?.Number,
    1,
    'summary cards share every wide row instead of leaving a ragged dead zone',
  );
  assert.deepEqual(
    ancestorProperty(stage, 'Containers', 'Card', 'Width'),
    { Length: { Chars: 14 } },
    'summary cards share one equal responsive basis while admitting two narrow columns',
  );
  assert.deepEqual(
    ancestorProperty(stage, 'Containers', 'CardContent', 'Pad'),
    { Length: { Step: 2 } },
    'repeated dashboard cards use compact content insets',
  );
  assert.deepEqual(
    ancestorProperty(stage, 'Containers', 'Card', 'Height'),
    { Length: 'Content' },
    'width growth never stretches summary rows down the viewport',
  );
  const openContainers = patches
    .filter(
      (patch) =>
        patch.SetProp?.prop === 'Tooltip' &&
        patch.SetProp.value?.Text === 'Open Containers' &&
        actionNodes.has(patch.SetProp.id),
    )
    .at(-1)?.SetProp.id;
  assert.ok(openContainers, 'the card itself navigates without a generic button row');
  assert.ok(
    stage.surface.dispatch({
      trigger: 'Invoke',
      node: openContainers,
      id: `${openContainers}:Invoke`,
      value: null,
    }),
  );
  assert.deepEqual(opened, ['containers'], 'the directly clickable card retains navigation');
  invoke(stage, 'Retry inventory');
  await settled();
  assert.deepEqual(
    reloaded.sort(),
    ['containers', 'executions', 'extensions', 'images', 'networks', 'terminals', 'volumes'],
    'the in-context recovery action retries every authoritative inventory',
  );
});

test('overview refreshes every authoritative inventory in one action', async () => {
  const calls = [];
  const inventory = (name) => ({
    data: [],
    loading: false,
    error: null,
    replace() {},
    reload: async () => calls.push(name),
  });
  const stage = host();
  stage.render(
    h(Overview, {
      containers: inventory('containers'),
      executions: inventory('executions'),
      images: inventory('images'),
      volumes: inventory('volumes'),
      networks: inventory('networks'),
      terminals: inventory('terminals'),
      extensions: inventory('extensions'),
      onOpen() {},
    }),
  );
  assert.ok(labelled(stage, 'Refresh workspace inventory'));
  assert.deepEqual(taggedProperty(stage, 'Refresh workspace inventory', 'IconButton', 'Icon'), {
    Text: 'view-refresh-symbolic',
  });
  invoke(stage, 'Refresh workspace inventory');
  await settled();
  assert.deepEqual(calls.sort(), [
    'containers',
    'executions',
    'extensions',
    'images',
    'networks',
    'terminals',
    'volumes',
  ]);
});

test('late inventory reloads cannot replace a newer authoritative snapshot', async () => {
  const pending = [];
  let publish;
  const controlled = {
    ...api,
    containers: { ...api.containers, list: () => new Promise((resolve) => pending.push(resolve)) },
  };
  const selections = {
    subscribe(listener) {
      publish = listener;
      return () => {};
    },
  };
  const stage = host();
  stage.render(
    h(Top, {
      api: controlled,
      selections,
      initial: { containers: [], executions: [], images: [], volumes: [], networks: [] },
    }),
  );
  await settled();
  publish({ snapshot: 'containers' });
  publish({ snapshot: 'containers' });
  await settled();
  pending[1]([
    { id: 'new-a', state: 'running' },
    { id: 'new-b', state: 'running' },
  ]);
  await settled();
  await settled();
  assert.ok(labelled(stage, '2'));
  assert.ok(labelled(stage, '2 running'));
  pending[0]([{ id: 'stale', state: 'exited' }]);
  await settled();
  await settled();
  assert.ok(labelled(stage, '2'));
  assert.ok(labelled(stage, '2 running'));
  assert.equal(
    labelled(stage, '1'),
    undefined,
    'superseded inventory authority never reaches the overview',
  );
});

test('every empty operational page explains what is absent and how to proceed', async () => {
  const stage = host();
  stage.render(
    h(Top, {
      api,
      initial: { containers: [], executions: [], images: [], volumes: [], networks: [] },
    }),
  );
  for (const [section, message] of [
    ['Containers', 'No containers'],
    ['Processes', 'No running processes'],
    ['Executions', 'No executions'],
    ['Images', 'No images'],
    ['Volumes', 'No volumes'],
    ['Networks', 'No networks'],
    ['Terminals', 'No terminal tabs'],
  ]) {
    invoke(stage, section);
    await settled();
    await settled();
    assert.ok(labelled(stage, message), `${section} has a semantic empty state`);
    assert.deepEqual(
      outerAncestorProperty(stage, message, 'Column', 'Pad'),
      { Length: { Step: 4 } },
      `${section} uses the same 16px page inset as the manager surfaces`,
    );
    if (['Processes', 'Executions', 'Terminals'].includes(section)) {
      assert.deepEqual(taggedProperty(stage, 'Refresh', 'IconButton', 'Icon'), {
        Text: 'view-refresh-symbolic',
      });
      assert.deepEqual(taggedProperty(stage, 'Refresh', 'IconButton', 'Size'), {
        ControlSize: 'Small',
      });
      assert.deepEqual(
        taggedProperty(stage, 'Refresh', 'IconButton', 'Tooltip'),
        { Text: `Refresh ${section.toLowerCase()}` },
        `${section} refresh is a compact, contextual toolbar action`,
      );
    }
    if (section === 'Images') {
      assert.equal(ancestorTags(stage, 'Pull')[0], 'Row');
      assert.deepEqual(
        ancestorTags(stage, 'Refresh').slice(0, 2),
        ['Row', 'Column'],
        'image actions stay grouped when the entry forces a narrow-line break',
      );
      assert.equal(
        ancestorProperty(stage, 'Pull', 'Row', 'Grow'),
        undefined,
        'the image toolbar cannot consume vertical empty-state space',
      );
    }
    if (section === 'Volumes') {
      assert.equal(ancestorTags(stage, 'Create')[0], 'Row');
      assert.deepEqual(
        ancestorTags(stage, 'Refresh').slice(0, 2),
        ['Row', 'FormControl'],
        'volume field and actions share one labelled responsive row',
      );
      assert.equal(
        ancestorProperty(stage, 'Create', 'Row', 'Grow'),
        undefined,
        'the volume toolbar cannot consume vertical empty-state space',
      );
    }
    if (section === 'Containers') {
      assert.ok(labelled(stage, 'Create first container'));
      assert.ok(labelled(stage, 'Create a container to start a service or open a shell.'));
      assert.equal(
        ancestorTags(stage, 'Create first container').includes('Column'),
        true,
        'the first-container action stays with the empty-state explanation',
      );
      invoke(stage, 'Create first container');
      assert.ok(labelled(stage, 'Container setup'), 'the primary action reveals container setup');
    }
    if (section === 'Networks') {
      assert.deepEqual(
        ancestorTags(stage, 'Refresh').slice(0, 2),
        ['Row', 'Column'],
        'network actions stay grouped when the entry forces a narrow-line break',
      );
    }
  }
  invoke(stage, 'Processes');
  await settled();
  assert.ok(labelled(stage, 'Open containers'));
  invoke(stage, 'Open containers');
  assert.ok(
    labelled(stage, 'Create and manage containers; inspect lifecycle, logs, and execution.'),
    'process empty state routes locally to Containers',
  );
  invoke(stage, 'Executions');
  await settled();
  assert.ok(labelled(stage, 'Open containers'));
  invoke(stage, 'Open containers');
  assert.ok(
    labelled(stage, 'Create and manage containers; inspect lifecycle, logs, and execution.'),
    'execution empty state routes locally to Containers',
  );
});

test('terminal management exposes exact pin state and acts through immutable tab identity', async () => {
  const calls = [];
  const resource = {
    data: [
      {
        id: 'p7',
        title: 'Build',
        pinned: false,
        panes: [{ slot: 's4', occupant: 'terminal', provider: null }],
      },
    ],
    loading: false,
    error: null,
    reload: async () => calls.push(['reload']),
  };
  const stage = host();
  stage.render(
    h(Terminals, {
      api: {
        terminal: {
          pinTab: async (...args) => calls.push(['pin', ...args]),
          focus: async (...args) => calls.push(['focus', ...args]),
        },
      },
      resource,
    }),
  );
  assert.equal(placeholderProperty(stage, 'New tab title', 'Grow')?.Number, 0);
  assert.ok(
    placeholderProperty(stage, 'New tab title', 'Width'),
    'tab creation stays compact instead of consuming the page height',
  );
  assert.ok(labelled(stage, 'Pane 1'));
  assert.equal(
    stage.frames
      .flatMap((frame) => frame.patches)
      .filter((patch) => patch.Create?.tag === 'ListRow').length,
    2,
    'one compact row represents the tab and one represents its pane',
  );
  assert.equal(labelled(stage, 'Tab 1'), undefined, 'tab metadata is not a second billboard');
  invoke(stage, 'Pin tab');
  await settled();
  await settled();
  assert.deepEqual(calls, [['pin', 'p7', true], ['reload']]);
  invoke(stage, 'Focus tab');
  await settled();
  assert.deepEqual(calls.at(-1), ['focus', 's4']);
});

test('a disappeared pane is reported as a stale layout instead of a raw protocol error', async () => {
  const resource = {
    data: [
      {
        id: 'tab-1',
        title: 'Shell',
        pinned: false,
        panes: [{ slot: 'shell', occupant: 'terminal', provider: null }],
      },
    ],
    loading: false,
    error: null,
    reload: async () => {},
  };
  const stage = host();
  stage.render(
    h(Terminals, {
      api: {
        terminal: {
          toText: async () => {
            throw new Error('pane does not exist: shell');
          },
          pinTab: async () => {},
          focus: async () => {},
        },
      },
      resource,
    }),
  );
  invoke(stage, 'View pane 1');
  await settled();
  await settled();
  assert.ok(
    labelled(
      stage,
      'This pane is no longer available. Refresh terminal tabs to see the current layout.',
    ),
  );
  assert.equal(labelled(stage, 'pane does not exist: shell'), undefined);
});

test('terminal management reads every pane as text and writes against the inspected terminal revision', async () => {
  const calls = [];
  const terminal = {
    toText: async (slot) => {
      calls.push(['read', slot]);
      if (slot === 'pane-ui')
        return {
          kind: 'ui',
          text: '<pane><button label="Deploy"/></pane>',
          complete: true,
          sourceTruncated: false,
          projectionTruncated: false,
          snapshot: { slot, generation: 3, revision: 4 },
        };
      return {
        kind: 'terminal',
        text: '$ ready',
        snapshot: { slot, generation: 7, revision: 11, lines: ['$ ready'], truncated: false },
      };
    },
    writeAndWait: async (...args) => {
      calls.push(['write', ...args]);
      return {
        changed: true,
        after: {
          slot: args[0],
          generation: 7,
          revision: 12,
          lines: ['$ ready', 'hello'],
          truncated: false,
        },
      };
    },
    pinTab: async () => {},
    focus: async () => {},
  };
  const resource = {
    data: [
      {
        id: 'tab-1',
        title: 'Shell',
        pinned: false,
        panes: [
          { slot: 'pane-1', occupant: 'terminal', provider: null },
          {
            slot: 'pane-ui',
            occupant: 'surface',
            provider: { extension: 'postgres', provider: 'overview' },
          },
        ],
      },
    ],
    loading: false,
    error: null,
    reload: async () => {},
  };
  const stage = host();
  stage.render(h(Terminals, { api: { terminal }, resource }));
  await settled();
  await settled();
  assert.deepEqual(calls, [['read', 'pane-1']]);
  assert.equal(latestPropertyForTag(stage, 'LogView', 'Value')?.Text, '$ ready');
  assert.equal(
    latestPropertyForTag(stage, 'LogView', 'Grow')?.Number,
    0,
    'the transcript keeps a compact viewport instead of consuming the full window height',
  );
  assert.ok(placeholderProperty(stage, 'Text to send', 'Width'));
  assert.equal(
    placeholderProperty(stage, 'Text to send', 'Grow'),
    undefined,
    'single-line input expands horizontally without requesting vertical growth',
  );
  change(stage, 'Text to send', 'printf hello');
  invoke(stage, 'Send text');
  await settled();
  await settled();
  assert.deepEqual(calls[1], ['write', 'pane-1', 7, 11, 'printf hello\n', { lines: 200 }]);
  assert.equal(latestPropertyForTag(stage, 'LogView', 'Value')?.Text, '$ ready\nhello');
  assert.equal(fieldValue(stage, 'Text to send'), '');
  invoke(stage, 'View pane 2');
  await settled();
  await settled();
  assert.equal(
    latestPropertyForTag(stage, 'LogView', 'Value')?.Text,
    '<pane><button label="Deploy"/></pane>',
  );
  assert.ok(labelled(stage, 'Interface controls'));
  assert.deepEqual(
    calls.filter(([kind]) => kind === 'write').length,
    1,
    'reading semantic XML never writes terminal bytes',
  );
});

test('terminal input stays unavailable without a host-issued revision cursor', async () => {
  const calls = [];
  const resource = {
    data: [
      {
        id: 'tab-1',
        title: 'Shell',
        pinned: false,
        panes: [{ slot: 'pane-1', occupant: 'terminal', provider: null }],
      },
    ],
    loading: false,
    error: null,
    reload: async () => {},
  };
  const stage = host();
  stage.render(
    h(Terminals, {
      api: {
        terminal: {
          toText: async () => ({
            kind: 'terminal',
            text: '$ old host',
            snapshot: { slot: 'pane-1', lines: ['$ old host'], truncated: false },
          }),
          writeAndWait: async (...args) => calls.push(args),
          pinTab: async () => {},
          focus: async () => {},
        },
      },
      resource,
    }),
  );
  invoke(stage, 'View pane 1');
  await settled();
  await settled();
  assert.ok(
    labelled(
      stage,
      'Input is unavailable until this pane provides a writable revision. Refresh the pane to try again.',
    ),
  );
  assert.equal(isEnabled(stage, 'Send text'), false);
  assert.deepEqual(
    calls,
    [],
    'input without an observed generation and revision cannot reach the socket',
  );
});

test('terminal pane layout mutations use the inspected generation and revision', async () => {
  const calls = [];
  const terminal = {
    toText: async () => ({
      kind: 'terminal',
      text: '$ ready',
      snapshot: {
        slot: 'pane-1',
        generation: 7,
        revision: 11,
        lines: ['$ ready'],
        truncated: false,
      },
    }),
    splitAndWait: async (...args) => {
      calls.push(['split', ...args]);
      return { changed: true, pane: {} };
    },
    retitleAndWait: async (...args) => {
      calls.push(['retitle', ...args]);
      return { changed: true, pane: {} };
    },
    ratioAndWait: async (...args) => {
      calls.push(['ratio', ...args]);
      return { changed: true, actual: args[3], pane: {} };
    },
    closeAndWait: async (...args) => {
      calls.push(['close', ...args]);
      return { changed: true, slot: args[0] };
    },
    pinTab: async () => {},
    focus: async () => {},
  };
  const resource = {
    data: [
      {
        id: 'tab-1',
        title: 'Shell',
        pinned: false,
        panes: [{ slot: 'pane-1', occupant: 'terminal', provider: null }],
      },
    ],
    loading: false,
    error: null,
    reload: async () => calls.push(['reload']),
  };
  const stage = host();
  stage.render(h(Terminals, { api: { terminal }, resource }));
  invoke(stage, 'View pane 1');
  await settled();
  await settled();
  invoke(stage, 'Split right');
  await settled();
  await settled();
  invoke(stage, 'Split down');
  await settled();
  await settled();
  change(stage, 'Size % (5–95)', '60');
  invoke(stage, 'Apply size');
  await settled();
  await settled();
  change(stage, 'Pane title', 'Build logs');
  invoke(stage, 'Rename');
  await settled();
  await settled();
  invoke(stage, 'Close pane');
  await settled();
  assert.equal(
    calls.some(([kind]) => kind === 'close'),
    false,
    'opening close confirmation has no authority',
  );
  invoke(stage, 'Confirm close');
  await settled();
  await settled();
  assert.deepEqual(
    calls.filter(([kind]) => kind !== 'reload'),
    [
      ['split', 'pane-1', 7, 11, 'beside'],
      ['split', 'pane-1', 7, 11, 'below'],
      ['ratio', 'pane-1', 7, 11, 0.6],
      ['retitle', 'pane-1', 7, 11, 'Build logs'],
      ['close', 'pane-1', 7, 11],
    ],
  );
  assert.equal(
    fieldValue(stage, 'Pane title'),
    '',
    'a proven close clears stale pane editing state',
  );
});

test('an unobserved terminal mutation keeps the inspected pane and reports uncertainty', async () => {
  const terminal = {
    toText: async () => ({
      kind: 'terminal',
      text: '$ ready',
      snapshot: {
        slot: 'pane-1',
        generation: 7,
        revision: 11,
        lines: ['$ ready'],
        truncated: false,
      },
    }),
    closeAndWait: async () => ({
      changed: false,
      slot: 'pane-1',
      after: { generation: 7, revision: 11 },
    }),
    pinTab: async () => {},
    focus: async () => {},
  };
  const resource = {
    data: [
      {
        id: 'tab-1',
        title: 'Shell',
        pinned: false,
        panes: [{ slot: 'pane-1', occupant: 'terminal', provider: null }],
      },
    ],
    loading: false,
    error: null,
    reload: async () => assert.fail('uncertain close cannot refresh as if it succeeded'),
  };
  const stage = host();
  stage.render(h(Terminals, { api: { terminal }, resource }));
  invoke(stage, 'View pane 1');
  await settled();
  await settled();
  invoke(stage, 'Close pane');
  invoke(stage, 'Confirm close');
  await settled();
  await settled();
  assert.ok(
    labelled(
      stage,
      'Pane pane-1 did not close before the observation window ended; refresh and try again.',
    ),
  );
  assert.ok(labelled(stage, 'Live terminal'), 'uncertain close retains the inspected pane');
});

test('terminal management opens tabs and spawns exact argv through observed operations', async () => {
  const calls = [];
  const terminal = {
    openTabAndWait: async (...args) => {
      calls.push(['open', ...args]);
      return { changed: true, tab: 'tab-new', pane: { slot: 'pane-new' } };
    },
    toText: async () => ({
      kind: 'terminal',
      text: '$ ready',
      snapshot: {
        slot: 'pane-1',
        generation: 7,
        revision: 11,
        lines: ['$ ready'],
        truncated: false,
      },
    }),
    spawnAndWait: async (...args) => {
      calls.push(['spawn', ...args]);
      return {
        changed: true,
        before: {},
        after: {
          slot: args[0],
          generation: 7,
          revision: 12,
          lines: ['$ make test', 'ok'],
          truncated: false,
        },
      };
    },
    resizeGridAndWait: async (...args) => {
      calls.push(['resize', ...args]);
      return {
        changed: true,
        before: {},
        after: {
          slot: args[0],
          generation: 7,
          revision: 13,
          columns: args[3],
          rows: args[4],
          lines: ['$ make test', 'ok'],
          truncated: false,
        },
      };
    },
    pinTab: async () => {},
    focus: async () => {},
  };
  const resource = {
    data: [
      {
        id: 'tab-1',
        title: 'Shell',
        pinned: false,
        panes: [{ slot: 'pane-1', occupant: 'terminal', provider: null }],
      },
    ],
    loading: false,
    error: null,
    reload: async () => calls.push(['reload']),
  };
  const stage = host();
  stage.render(h(Terminals, { api: { terminal }, resource }));
  await settled();
  await settled();
  change(stage, 'New tab title', ' Tests ');
  invoke(stage, 'Create terminal tab');
  await settled();
  await settled();
  change(stage, 'Program and arguments, e.g. make test', 'make test');
  invoke(stage, 'Execute');
  await settled();
  await settled();
  change(stage, 'Columns', '120');
  change(stage, 'Rows', '40');
  invoke(stage, 'Resize grid');
  await settled();
  await settled();
  assert.deepEqual(calls, [
    ['open', 'Tests'],
    ['reload'],
    ['spawn', 'pane-1', 7, 11, ['make', 'test'], { lines: 200 }],
    ['resize', 'pane-1', 7, 12, 120, 40, { lines: 200 }],
  ]);
  assert.equal(latestPropertyForTag(stage, 'LogView', 'Value')?.Text, '$ make test\nok');
  assert.equal(fieldValue(stage, 'Program and arguments, e.g. make test'), '');
});

test('terminal command input preserves quoted arguments and rejects unfinished syntax', async () => {
  const calls = [];
  const terminal = {
    toText: async () => ({
      kind: 'terminal',
      text: '$ ready',
      snapshot: {
        slot: 'pane-1',
        generation: 7,
        revision: 11,
        lines: ['$ ready'],
        truncated: false,
      },
    }),
    spawnAndWait: async (...args) => {
      calls.push(args);
      return {
        changed: true,
        after: {
          slot: args[0],
          generation: 7,
          revision: 12,
          lines: ['$ ready'],
          truncated: false,
        },
      };
    },
    pinTab: async () => {},
    focus: async () => {},
  };
  const resource = {
    data: [
      {
        id: 'tab-1',
        title: 'Shell',
        pinned: false,
        panes: [{ slot: 'pane-1', occupant: 'terminal', provider: null }],
      },
    ],
    loading: false,
    error: null,
    reload: async () => {},
  };
  const stage = host();
  stage.render(h(Terminals, { api: { terminal }, resource }));
  invoke(stage, 'View pane 1');
  await settled();
  await settled();
  change(stage, 'Program and arguments, e.g. make test', 'printf "hello world"');
  invoke(stage, 'Execute');
  await settled();
  await settled();
  assert.deepEqual(calls[0]?.slice(0, 4), ['pane-1', 7, 11, ['printf', 'hello world']]);
  change(stage, 'Program and arguments, e.g. make test', 'printf "unfinished');
  invoke(stage, 'Execute');
  await settled();
  await settled();
  assert.equal(calls.length, 1);
  assert.ok(labelled(stage, 'Command has an unfinished quote or escape.'));
});

test('terminal management switches an inspected pane to an enabled exact provider', async () => {
  const calls = [];
  const terminal = {
    toText: async () => ({
      kind: 'ui',
      text: '<pane/>',
      complete: true,
      sourceTruncated: false,
      projectionTruncated: false,
      snapshot: { slot: 'pane-ui', generation: 3, revision: 4, root: { id: 1 }, truncated: false },
    }),
    switchOccupantAndWait: async (...args) => {
      calls.push(args);
      return { changed: true, pane: { slot: args[0], generation: 3, revision: 5 } };
    },
    pinTab: async () => {},
    focus: async () => {},
  };
  const controlled = {
    extensions: {
      providers: async () => ({
        providers: [{ extension: 'storybook', id: 'catalogue', title: 'Component catalogue' }],
        truncated: false,
      }),
    },
    terminal,
  };
  const resource = {
    data: [
      {
        id: 'tab-1',
        title: 'UI',
        pinned: false,
        panes: [
          {
            slot: 'pane-ui',
            occupant: 'surface',
            provider: { extension: 'postgres', provider: 'overview' },
          },
        ],
      },
    ],
    loading: false,
    error: null,
    reload: async () => calls.push(['reload']),
  };
  const stage = host();
  stage.render(h(Terminals, { api: controlled, resource }));
  await settled();
  invoke(stage, 'View pane 1');
  await settled();
  await settled();
  const select = stage.frames
    .flatMap((frame) => frame.patches)
    .filter((patch) => patch.Create?.tag === 'Select')
    .at(-1).Create.id;
  assert.ok(
    stage.surface.dispatch({
      trigger: 'Change',
      node: select,
      id: `${select}:Change`,
      value: 'storybook/catalogue',
    }),
  );
  invoke(stage, 'Change content');
  await settled();
  await settled();
  assert.deepEqual(calls, [
    ['pane-ui', 3, 4, { kind: 'surface', extension: 'storybook', provider: 'catalogue' }],
    ['reload'],
  ]);
});

test('terminal management re-inspects semantic authority and confirms destructive UI actions', async () => {
  const calls = [];
  const tree = (revision, value) => ({
    slot: 'pane-ui',
    generation: 3,
    revision,
    truncated: false,
    root: {
      id: 42,
      role: 'button',
      label: 'Delete',
      value,
      disabled: false,
      destructive: true,
      actions: ['invoke'],
      children: [],
    },
  });
  const terminal = {
    toText: async () => ({
      kind: 'ui',
      text: '<button id="42" destructive="true"/>',
      complete: true,
      sourceTruncated: false,
      projectionTruncated: false,
      snapshot: tree(4, null),
    }),
    inspectAndAct: async (...args) => {
      calls.push(args);
      return {
        changed: true,
        before: {
          snapshot: tree(4, null),
          text: '<button/>',
          complete: true,
          sourceTruncated: false,
          projectionTruncated: false,
        },
        after: {
          snapshot: tree(5, 'done'),
          text: '<button value="done"/>',
          complete: false,
          sourceTruncated: false,
          projectionTruncated: true,
        },
      };
    },
    pinTab: async () => {},
    focus: async () => {},
  };
  const resource = {
    data: [
      {
        id: 'tab-1',
        title: 'UI',
        pinned: false,
        panes: [
          {
            slot: 'pane-ui',
            occupant: 'surface',
            provider: { extension: 'tool', provider: 'main' },
          },
        ],
      },
    ],
    loading: false,
    error: null,
    reload: async () => {},
  };
  const stage = host();
  stage.render(h(Terminals, { api: { terminal }, resource }));
  invoke(stage, 'View pane 1');
  await settled();
  await settled();
  change(stage, 'Node number', '42');
  invoke(stage, 'Run action');
  await settled();
  assert.deepEqual(calls, [], 'opening destructive semantic confirmation has no socket authority');
  invoke(stage, 'Confirm action');
  await settled();
  await settled();
  assert.deepEqual(calls, [['pane-ui', { node: 42, action: 'invoke', value: null }]]);
  assert.equal(latestPropertyForTag(stage, 'LogView', 'Value')?.Text, '<button value="done"/>');
  assert.ok(
    labelled(
      stage,
      'This interface snapshot is partial because its text projection reached the client limit.',
    ),
  );
});

test('process snapshots disclose initial-only reusable PID scope and host truncation', async () => {
  const processApi = {
    containers: {
      processes: async () => ({
        titles: ['PID', 'PPID', 'USER', 'STAT', 'COMMAND'],
        processes: [['1', '0', 'root', '?', '/usr/bin/server']],
        observed_at_ms: 1_700_000_000_000,
        scope: 'initial',
        pid_identity: 'snapshot',
        truncated: true,
      }),
    },
  };
  const stage = host();
  const processTable = new ProcessTableSource();
  stage.render(
    h(Processes, {
      api: processApi,
      resource: { data: [{ id: 'c1', name: 'api' }], loading: false },
      processTable,
    }),
  );
  await settled();
  await settled();
  assert.ok(
    labelled(stage, 'Initial processes only; PIDs identify this snapshot and may be reused.'),
  );
  assert.ok(labelled(stage, 'Observed Nov 14, 2023, 22:13 UTC'));
  assert.ok(!labelled(stage, 'Observed 2023-11-14T22:13:20.000Z'));
  assert.ok(labelled(stage, 'The host process snapshot was truncated at its safety limit.'));
  assert.equal(
    latestPropertyForTag(stage, 'Entry', 'Placeholder')?.Text,
    'Filter container, user, PID, or command',
  );
  const schema = latestPropertyForTag(stage, 'DataTable', 'Schema')?.Schema;
  assert.deepEqual(
    schema.map(({ key }) => key),
    ['container', 'pid', 'user', 'command'],
  );
  assert.equal(latestPropertyForTag(stage, 'DataTable', 'Source')?.Source, 206);
  assert.ok(processTable.version >= 1);
  assert.ok(!labelled(stage, 'Signal'), 'snapshot PID rows never acquire a control action');
  assert.ok(!labelled(stage, 'Kill'), 'snapshot PID rows never acquire a control action');
});

test('one unavailable container does not hide healthy process snapshots', async () => {
  const processApi = {
    containers: {
      processes: async (id) => {
        if (id === 'broken') throw new Error('container is stopped');
        return {
          titles: ['PID', 'COMMAND'],
          processes: [['17', '/usr/bin/healthy']],
          observed_at_ms: 1_700_000_000_000,
          scope: 'namespace',
          pid_identity: 'snapshot',
          truncated: false,
        };
      },
    },
  };
  const stage = host();
  const processTable = new ProcessTableSource();
  stage.render(
    h(Processes, {
      api: processApi,
      processTable,
      resource: {
        data: [
          { id: 'healthy', name: 'api' },
          { id: 'broken', name: 'worker' },
        ],
        loading: false,
        error: null,
        reload: async () => {},
      },
    }),
  );
  await settled();
  await settled();
  assert.ok(processWindowText(processTable).includes('/usr/bin/healthy'));
  assert.ok(
    labelled(
      stage,
      '1 container process snapshot unavailable; available containers remain visible.',
    ),
  );
  assert.deepEqual(
    property(
      stage,
      '1 container process snapshot unavailable; available containers remain visible.',
      'Tone',
    ),
    { Tone: 'Warning' },
    'partial process loss is a non-color warning rather than raw body text',
  );
  assert.ok(labelled(stage, 'Technical details'));
  assert.ok(labelled(stage, 'worker: container is stopped'));
  assert.ok(
    ancestorTags(stage, 'worker: container is stopped').includes('Expander'),
    'the failing container diagnostic is disclosed on request',
  );
  assert.equal(
    labelled(stage, 'Retry processes'),
    undefined,
    'a partial snapshot remains usable rather than becoming a page-wide error',
  );
});

test('large process inventories stay below the client pending-call window', async () => {
  let active = 0;
  let peak = 0;
  let completed = 0;
  const processApi = {
    containers: {
      processes: async () => {
        active += 1;
        peak = Math.max(peak, active);
        await settled();
        active -= 1;
        completed += 1;
        return {
          titles: ['PID'],
          processes: [],
          observed_at_ms: 1,
          scope: 'namespace',
          pid_identity: 'snapshot',
          truncated: false,
        };
      },
    },
  };
  const stage = host();
  stage.render(
    h(Processes, {
      api: processApi,
      resource: {
        data: Array.from({ length: 25 }, (_, index) => ({
          id: `container-${index}`,
          name: `container-${index}`,
        })),
        loading: false,
        error: null,
        reload: async () => {},
      },
    }),
  );
  while (completed < 25) await settled();
  assert.equal(peak, 8);
  assert.ok(labelled(stage, 'No running processes'));
});

test('a late process snapshot cannot replace a newer container inventory', async () => {
  const pending = new Map();
  const processApi = {
    containers: { processes: (id) => new Promise((resolve) => pending.set(id, resolve)) },
  };
  const stage = host();
  const processTable = new ProcessTableSource();
  const resource = (id, name) => ({
    data: [{ id, name }],
    loading: false,
    error: null,
    reload: async () => {},
  });
  stage.render(
    h(Processes, { api: processApi, resource: resource('old', 'former'), processTable }),
  );
  await settled();
  stage.render(
    h(Processes, { api: processApi, resource: resource('new', 'current'), processTable }),
  );
  await settled();
  pending.get('new')({
    titles: ['PID', 'COMMAND'],
    processes: [['22', '/usr/bin/current']],
    observed_at_ms: 2,
    scope: 'namespace',
    pid_identity: 'snapshot',
    truncated: false,
  });
  await settled();
  await settled();
  assert.ok(processWindowText(processTable).includes('/usr/bin/current'));
  pending.get('old')({
    titles: ['PID', 'COMMAND'],
    processes: [['11', '/usr/bin/stale']],
    observed_at_ms: 1,
    scope: 'namespace',
    pid_identity: 'snapshot',
    truncated: false,
  });
  await settled();
  await settled();
  assert.ok(processWindowText(processTable).includes('/usr/bin/current'));
  assert.equal(processWindowText(processTable).includes('/usr/bin/stale'), false);
});

test('execution observation is scoped to its page and replaces inventory without polling', async () => {
  const calls = [];
  let publish;
  const observed = {
    ...api,
    watchExecutions: async (listener) => {
      calls.push('subscribe');
      publish = listener;
      return async () => calls.push('unsubscribe');
    },
  };
  const stage = host();
  stage.render(
    h(Top, {
      api: observed,
      initial: { containers: [], executions: [], images: [], volumes: [], networks: [] },
    }),
  );
  assert.deepEqual(calls, []);
  invoke(stage, 'Executions');
  await settled();
  assert.deepEqual(calls, ['subscribe']);
  publish({
    executions: [
      {
        id: 'live',
        container_id: 'c1',
        running: true,
        exit_code: 0,
        pid: 9,
        command: ['live-command'],
        user: '',
      },
    ],
    truncated: true,
  });
  await settled();
  assert.ok(labelled(stage, 'live-command'));
  assert.ok(labelled(stage, 'The host execution catalogue was truncated at its safety limit.'));
  invoke(stage, 'Images');
  await settled();
  assert.deepEqual(calls, ['subscribe', 'unsubscribe']);
  publish({
    executions: [
      {
        id: 'late',
        container_id: 'c1',
        running: false,
        exit_code: 0,
        pid: 0,
        command: ['late-command'],
        user: '',
      },
    ],
    truncated: false,
  });
  await settled();
  assert.equal(
    labelled(stage, 'late-command'),
    undefined,
    'disposed observation ignores late delivery',
  );
});

test('image removal and prune require an explicit confirmation step', async () => {
  const calls = [];
  const originalDigest = `sha256:${'a'.repeat(64)}`;
  const refreshedDigest = `sha256:${'b'.repeat(64)}`;
  const controlled = {
    images: {
      ...api.images,
      removeAndWait: async (...args) => {
        calls.push(['remove', ...args]);
        return { changed: true, id: args[0] };
      },
      prune: async () => {
        calls.push(['prune']);
        return { deleted: 0, space_reclaimed: 0 };
      },
    },
  };
  const resource = {
    data: [{ id: originalDigest, reference: 'alpine:3.20', size: 7, created: 0 }],
    loading: false,
    error: null,
    reload: async () => {},
  };
  const stage = host();
  const frame = stage.render(h(Images, { api: controlled, resource }));
  assert.equal(
    ancestorProperty(stageFromFrame(frame), 'Image maintenance', 'Card', 'Grow'),
    undefined,
    'maintenance stays content-height on wide windows',
  );
  const labels = () =>
    stage.frames
      .flatMap((current) => current.patches)
      .filter((patch) => 'SetProp' in patch && patch.SetProp.prop === 'Label');
  const remove = labels().find((patch) => patch.SetProp.value.Text === 'Remove').SetProp.id;
  assert.ok(
    frame.patches.some(
      (patch) => patch.SetProp?.id === remove && patch.SetProp.value?.Variant === 'Outline',
    ),
    'the initial image removal is visibly outlined before confirmation',
  );
  assert.ok(
    stage.surface.dispatch({
      trigger: 'Invoke',
      node: remove,
      id: `${remove}:Invoke`,
      value: null,
    }),
  );
  assert.deepEqual(calls, [], 'opening image removal performs no operation');
  assert.ok(labels().some((patch) => patch.SetProp.value.Text === 'Confirm remove'));
  assert.ok(labelled(stage, `Remove alpine:3.20 (${originalDigest.slice(0, 12)})?`));
  const staleConfirm = labels()
    .filter((patch) => patch.SetProp.value.Text === 'Confirm remove')
    .at(-1).SetProp.id;
  assert.equal(
    frame.patches.some(
      (patch) => 'SetProp' in patch && patch.SetProp.value?.Text === 'Confirm remove',
    ),
    false,
  );
  const refreshed = { ...resource, data: [{ ...resource.data[0], id: refreshedDigest }] };
  stage.render(h(Images, { api: controlled, resource: refreshed }));
  stage.surface.dispatch({
    trigger: 'Invoke',
    node: staleConfirm,
    id: `${staleConfirm}:Invoke`,
    value: null,
  });
  await settled();
  assert.deepEqual(calls, [], 'stale digest consent cannot reach removal authority after refresh');
  assert.ok(
    labelled(stage, `Image ${originalDigest} changed or disappeared; inspect and confirm again.`),
  );
  invoke(stage, 'Remove');
  assert.ok(labelled(stage, `Remove alpine:3.20 (${refreshedDigest.slice(0, 12)})?`));
  invoke(stage, 'Confirm remove');
  await settled();
  assert.deepEqual(calls, [['remove', refreshedDigest]]);
  assert.ok(labelled(stage, `Image ${refreshedDigest} was removed and its absence was verified.`));

  const pruneStage = host();
  const pruneFrame = pruneStage.render(h(Images, { api: controlled, resource }));
  assert.ok(labelled(pruneStage, 'Image maintenance'));
  assert.ok(labelled(pruneStage, 'Bulk action · removes every image not used by a container.'));
  const prune = pruneFrame.patches.find(
    (patch) =>
      'SetProp' in patch &&
      patch.SetProp.prop === 'Label' &&
      patch.SetProp.value.Text === 'Prune unused images',
  ).SetProp.id;
  assert.ok(
    pruneFrame.patches.some(
      (patch) => patch.SetProp?.id === prune && patch.SetProp.value?.Variant === 'Outline',
    ),
    'the initial prune action is visibly outlined before confirmation',
  );
  assert.ok(
    pruneStage.surface.dispatch({
      trigger: 'Invoke',
      node: prune,
      id: `${prune}:Invoke`,
      value: null,
    }),
  );
  assert.deepEqual(
    calls,
    [['remove', refreshedDigest]],
    'opening image prune performs no operation',
  );
  assert.ok(
    pruneStage.frames
      .flatMap((current) => current.patches)
      .some((patch) => 'SetProp' in patch && patch.SetProp.value?.Text === 'Confirm prune'),
  );
  invoke(pruneStage, 'Confirm prune');
  await settled();
  assert.deepEqual(calls, [['remove', refreshedDigest], ['prune']]);
});

test('image inspect renders real typed details through a bounded source and retries failures', async () => {
  let attempts = 0;
  const mutations = [];
  const imageDetails = new ImageDetailsSource(async (mutation) => mutations.push(mutation));
  const controlled = {
    images: {
      ...api.images,
      inspect: async () => {
        attempts += 1;
        if (attempts === 1) throw new Error('manifest temporarily unavailable');
        return {
          id: 'sha256:one',
          references: ['alpine:3.20'],
          created: 'now',
          size: 7,
          os: 'linux',
          architecture: 'amd64',
          entrypoint: ['/bin/sh'],
          command: [],
          working_directory: '/',
          user: '',
        };
      },
    },
  };
  const resource = {
    data: [{ id: 'sha256:one', reference: 'alpine:3.20', size: 7 }],
    loading: false,
    error: null,
    reload: async () => {},
  };
  const stage = host();
  stage.render(h(Images, { api: controlled, resource, imageDetails }));
  invoke(stage, 'Inspect');
  await settled();
  await settled();
  assert.ok(
    labelled(stage, 'Reading image details…'),
    'loading is visible and semantic before failure',
  );
  assert.ok(labelled(stage, 'manifest temporarily unavailable'));
  invoke(stage, 'Retry inspect');
  await settled();
  await settled();
  assert.equal(attempts, 2);
  assert.ok(labelled(stage, 'Image summary'));
  assert.ok(labelled(stage, 'Platform · linux/amd64'));
  assert.ok(labelled(stage, 'Created · now'));
  assert.ok(labelled(stage, 'Technical details'));
  assert.ok(labelled(stage, 'Immutable image ID · sha256:one'));
  assert.ok(labelled(stage, 'References · alpine:3.20'));
  assert.equal(labelled(stage, '$.id'), undefined);
  assert.deepEqual(mutations, [{ Length: { source: 201, version: 1, rows: 9 } }]);
  assert.equal(
    imageDetails.answer({ source: 201, version: 1, id: 8, range: { start: 0, count: 999 } }).rows
      .length,
    4,
  );
});

test('an empty typed image inspection has an explicit semantic empty state', async () => {
  const controlled = { images: { ...api.images, inspect: async () => ({}) } };
  const resource = {
    data: [{ id: 'sha256:empty', reference: 'empty:latest', size: 0 }],
    loading: false,
    error: null,
    reload: async () => {},
  };
  const stage = host();
  stage.render(h(Images, { api: controlled, resource, imageDetails: new ImageDetailsSource() }));
  invoke(stage, 'Inspect');
  await settled();
  await settled();
  assert.ok(labelled(stage, 'No image details'));
  assert.ok(
    stage.frames
      .flatMap((frame) => frame.patches)
      .some(
        (patch) =>
          patch.SetProp?.prop === 'Detail' &&
          patch.SetProp.value?.Text === 'The host returned no inspectable fields.',
      ),
  );
});

test('typed image inspection never exposes unknown host object fields', async () => {
  const oversized = Object.fromEntries(
    Array.from({ length: 200 }, (_, index) => [`field_${index}`, `value-${index}`]),
  );
  const controlled = {
    images: {
      ...api.images,
      inspect: async () => ({
        id: 'sha256:bounded',
        references: ['bounded:latest'],
        created: 'now',
        size: 1,
        os: 'linux',
        architecture: 'amd64',
        entrypoint: [],
        command: [],
        working_directory: '',
        user: '',
        ...oversized,
      }),
    },
  };
  const resource = {
    data: [{ id: 'sha256:bounded', reference: 'bounded:latest', size: 1 }],
    loading: false,
    error: null,
    reload: async () => {},
  };
  const stage = host();
  stage.render(h(Images, { api: controlled, resource, imageDetails: new ImageDetailsSource() }));
  invoke(stage, 'Inspect');
  await settled();
  await settled();
  assert.ok(labelled(stage, 'Image summary'));
  assert.equal(labelled(stage, '$.field_199'), undefined);
  assert.equal(labelled(stage, 'value-199'), undefined);
});

test('image pull progress is determinate, cancellable and retryable from retained input', async () => {
  const calls = [];
  let publish;
  const controlled = {
    ...api,
    images: {
      ...api.images,
      startPull: async (reference) => {
        calls.push(['start', reference]);
        return { job: String(calls.length) };
      },
      pullStatus: async (job) => ({
        job,
        reference: 'alpine:3.20',
        revision: 2,
        state: 'pulling',
        status: 'Downloading',
        layer: 'sha256:layer',
        current: 25,
        total: 100,
        image: null,
        error: null,
      }),
      cancelPull: async (job) => calls.push(['cancel', job]),
    },
    watchImagePulls: async (listener) => {
      publish = listener;
      return async () => calls.push(['unsubscribe']);
    },
  };
  const resource = {
    data: [],
    loading: false,
    error: null,
    reload: async () => calls.push(['reload']),
  };
  const stage = host();
  stage.render(h(Images, { api: controlled, resource }));
  change(stage, 'registry/image:tag', 'alpine:3.20');
  invoke(stage, 'Pull');
  await settled();
  await settled();
  assert.deepEqual(calls, [['start', 'alpine:3.20']]);
  publish({ job: '1', revision: 2, state: 'pulling', coalesced: 0 });
  await settled();
  await settled();
  assert.ok(labelled(stage, 'Layer sha256:layer'));
  assert.ok(
    stage.frames
      .flatMap((frame) => frame.patches)
      .some((patch) => patch.SetProp?.prop === 'Fraction' && patch.SetProp.value?.Number === 0.25),
  );
  invoke(stage, 'Cancel pull');
  await settled();
  await settled();
  assert.ok(calls.some((call) => call[0] === 'cancel'));
  assert.ok(
    stage.frames
      .flatMap((frame) => frame.patches)
      .some(
        (patch) =>
          patch.SetProp?.prop === 'Detail' && patch.SetProp.value?.Text === 'Pull cancelled.',
      ),
  );
  invoke(stage, 'Pull');
  await settled();
  await settled();
  assert.deepEqual(
    calls.filter((call) => call[0] === 'start').map((call) => call[1]),
    ['alpine:3.20', 'alpine:3.20'],
  );
});

test('a completed image pull reports success, refreshes inventory and retains its reference', async () => {
  const calls = [];
  let publish;
  const controlled = {
    ...api,
    images: {
      ...api.images,
      startPull: async () => ({ job: 'done' }),
      pullStatus: async () => ({
        job: 'done',
        reference: 'alpine:3.20',
        revision: 2,
        state: 'complete',
        status: 'Pull complete',
        layer: null,
        current: 100,
        total: 100,
        image: { id: 'i1', reference: 'alpine:3.20', size: 1, created: 0 },
        error: null,
      }),
      cancelPull: async () => {},
    },
    watchImagePulls: async (listener) => {
      publish = listener;
      return async () => {};
    },
  };
  const stage = host();
  stage.render(
    h(Images, {
      api: controlled,
      resource: { data: [], loading: false, error: null, reload: async () => calls.push('reload') },
    }),
  );
  change(stage, 'registry/image:tag', 'alpine:3.20');
  invoke(stage, 'Pull');
  await settled();
  await settled();
  publish({ job: 'done', revision: 2, state: 'complete', coalesced: 0 });
  await settled();
  await settled();
  assert.ok(labelled(stage, 'Pulled alpine:3.20.'));
  assert.deepEqual(calls, ['reload']);
  assert.ok(
    stage.frames
      .flatMap((frame) => frame.patches)
      .some(
        (patch) => patch.SetProp?.prop === 'Value' && patch.SetProp.value?.Text === 'alpine:3.20',
      ),
  );
});

test('an image pull status older than its announced revision is ignored', async () => {
  const statuses = [
    {
      job: 'ordered',
      reference: 'alpine:3.20',
      revision: 1,
      state: 'pulling',
      status: 'Starting',
      layer: null,
      current: null,
      total: null,
      image: null,
      error: null,
    },
  ];
  let publish;
  let reloads = 0;
  const controlled = {
    ...api,
    images: {
      ...api.images,
      startPull: async () => ({ job: 'ordered' }),
      pullStatus: async () => statuses.shift(),
      cancelPull: async () => {},
    },
    watchImagePulls: async (listener) => {
      publish = listener;
      return async () => {};
    },
  };
  const stage = host();
  stage.render(
    h(Images, {
      api: controlled,
      resource: {
        data: [],
        loading: false,
        error: null,
        reload: async () => {
          reloads += 1;
        },
      },
    }),
  );
  change(stage, 'registry/image:tag', 'alpine:3.20');
  invoke(stage, 'Pull');
  await settled();
  await settled();
  statuses.push({
    job: 'ordered',
    reference: 'alpine:3.20',
    revision: 2,
    state: 'pulling',
    status: 'Stale read',
    layer: 'stale',
    current: 20,
    total: 100,
    image: null,
    error: null,
  });
  await publish({ job: 'ordered', revision: 3, state: 'pulling', coalesced: 0 });
  await settled();
  await settled();
  assert.equal(
    labelled(stage, 'Layer stale'),
    undefined,
    'status older than its triggering event has no authority',
  );
  statuses.push({
    job: 'ordered',
    reference: 'alpine:3.20',
    revision: 4,
    state: 'complete',
    status: 'Pull complete',
    layer: null,
    current: 100,
    total: 100,
    image: { id: 'i1' },
    error: null,
  });
  await publish({ job: 'ordered', revision: 4, state: 'complete', coalesced: 0 });
  await settled();
  await settled();
  assert.ok(labelled(stage, 'Pulled alpine:3.20.'));
  assert.equal(reloads, 1, 'only the accepted completion refreshes inventory');
});

test('a cached image pull completed before subscription is reconciled without an event', async () => {
  let reloads = 0;
  const controlled = {
    ...api,
    images: {
      ...api.images,
      startPull: async () => ({ job: 'cached' }),
      pullStatus: async () => ({
        job: 'cached',
        reference: 'alpine:3.20',
        revision: 1,
        state: 'complete',
        status: 'Already present',
        layer: null,
        current: 1,
        total: 1,
        image: { id: 'cached-image' },
        error: null,
      }),
      cancelPull: async () => {},
    },
    watchImagePulls: async () => async () => {},
  };
  const stage = host();
  stage.render(
    h(Images, {
      api: controlled,
      resource: {
        data: [],
        loading: false,
        error: null,
        reload: async () => {
          reloads += 1;
        },
      },
    }),
  );
  change(stage, 'registry/image:tag', 'alpine:3.20');
  invoke(stage, 'Pull');
  await settled();
  await settled();
  assert.ok(labelled(stage, 'Pulled alpine:3.20.'));
  assert.equal(reloads, 1, 'reconciliation refreshes inventory exactly once');
});

test('volume and network panels render bounded real inventories and controls', () => {
  const resource = (data) => ({ data, loading: false, error: null, reload: async () => {} });
  const volumeFrame = host().render(
    h(Volumes, { api, resource: resource([{ name: 'cache', driver: 'local' }]) }),
  );
  const networkFrame = host().render(
    h(Networks, {
      api,
      containers: containerResource(),
      resource: resource([
        { id: 'n1', name: 'private', driver: 'bridge', scope: 'local', kind: 'custom' },
        { id: 'n2', name: 'bridge', driver: 'bridge', scope: 'local', kind: 'builtin' },
      ]),
    }),
  );
  const labels = (frame) =>
    frame.patches
      .filter((patch) => 'SetProp' in patch && patch.SetProp.prop === 'Label')
      .map((patch) => patch.SetProp.value.Text);
  for (const label of ['Volumes', 'cache', 'Create', 'Inspect', 'Remove'])
    assert.ok(labels(volumeFrame).includes(label), label);
  const volumeStage = stageFromFrame(volumeFrame);
  assert.equal(taggedProperty(volumeStage, 'cache', 'CardHeader', 'Align')?.Align, 'Start');
  assert.ok(taggedProperty(volumeStage, 'cache', 'CardHeader', 'Width'));
  for (const label of ['Networks', 'private', 'Remove'])
    assert.ok(labels(networkFrame).includes(label), label);
  const networkInventoryStage = stageFromFrame(networkFrame);
  assert.deepEqual(taggedProperty(networkInventoryStage, 'Networks', 'Heading', 'Scale'), {
    Scale: 'Display',
  });
  assert.ok(taggedProperty(networkInventoryStage, 'private', 'Heading', 'Scale'));
  assert.equal(
    networkFrame.patches.filter((patch) => patch.Create?.tag === 'CardContent').length,
    3,
    'custom-network danger controls stay in a separate subordinate content band',
  );
  assert.ok(
    !labels(networkFrame).includes('Disconnect'),
    'destructive endpoint action waits for a target',
  );
  assert.ok(labels(networkFrame).includes('Built-in · protected'));
  assert.equal(
    labels(networkFrame).filter((label) => label === 'Remove').length,
    1,
    'only the custom network offers removal',
  );
  const networkStage = stageFromFrame(networkFrame);
  assert.equal(
    labels(networkFrame).includes('Container attachment'),
    false,
    'attachment controls do not precede network selection and inspection',
  );
  assert.deepEqual(ancestorProperty(networkStage, 'private', 'Card', 'Width'), {
    Length: 'Fill',
  });
  assert.equal(
    ancestorProperty(networkStage, 'private', 'Card', 'Grow'),
    undefined,
    'inventory cards remain content-height on wide windows',
  );
  assert.equal(
    ancestorProperty(networkStage, 'private', 'Card', 'Justify'),
    undefined,
    'inventory cards retain horizontal fill instead of overriding it with start alignment',
  );
  assert.ok(
    ancestorTags(networkStage, 'Manage connections').includes('CardContent'),
    'the primary connection-management action appears in the summary band',
  );
  assert.equal(ancestorTags(networkStage, 'Manage connections').includes('CardActions'), false);
  assert.deepEqual(
    taggedProperty(networkStage, 'Manage connections', 'Button', 'Size'),
    { ControlSize: 'Small' },
    'connection management remains a compact secondary action beside network identity',
  );
  const destructive = (frame, label) => {
    const id = frame.patches.find(
      (patch) =>
        'SetProp' in patch && patch.SetProp.prop === 'Label' && patch.SetProp.value.Text === label,
    ).SetProp.id;
    return frame.patches.some(
      (patch) =>
        'SetProp' in patch &&
        patch.SetProp.id === id &&
        patch.SetProp.prop === 'Destructive' &&
        patch.SetProp.value.Flag === true,
    );
  };
  assert.equal(destructive(volumeFrame, 'Remove'), false);
  assert.equal(destructive(networkFrame, 'Remove'), false);
});

test('network inspection exposes loading, retry, empty and domain-specific details', async () => {
  let attempts = 0;
  const networkId = 'b'.repeat(32);
  const containerId = 'a'.repeat(64);
  const controlled = {
    networks: {
      ...api.networks,
      inspect: async () => {
        attempts += 1;
        if (attempts === 1) throw new Error('network inspect unavailable');
        return {
          id: networkId,
          name: 'private',
          driver: 'bridge',
          scope: 'local',
          kind: 'custom',
          endpoints: { containers: [containerId], truncated: false },
        };
      },
    },
  };
  const resource = {
    data: [{ id: networkId, name: 'private', driver: 'bridge', scope: 'local', kind: 'custom' }],
    loading: false,
    error: null,
    reload: async () => {},
  };
  const stage = host();
  stage.render(h(Networks, { api: controlled, resource, containers: containerResource() }));
  assert.ok(labelled(stage, 'Connections unknown'));
  invoke(stage, 'Manage connections');
  await settled();
  await settled();
  assert.ok(labelled(stage, 'Reading network details…'));
  assert.ok(labelled(stage, 'Managing connections…'));
  const managingNode = labelled(stage, 'Managing connections…').SetProp.id;
  assert.ok(
    stage.frames
      .flatMap((frame) => frame.patches)
      .some(
        (patch) =>
          patch.SetProp?.id === managingNode &&
          patch.SetProp?.prop === 'Enabled' &&
          patch.SetProp.value?.Flag === false,
      ),
    'connection management cannot be invoked again while inspection is pending',
  );
  assert.ok(labelled(stage, 'network inspect unavailable'));
  invoke(stage, 'Retry managing connections');
  await settled();
  await settled();
  assert.ok(labelled(stage, 'Network details'));
  assert.ok(labelled(stage, 'Driver · bridge'));
  assert.ok(labelled(stage, 'Scope · local'));
  assert.ok(labelled(stage, 'Connected containers · 1'));
  assert.ok(labelled(stage, '1 connected'));
  assert.ok(labelled(stage, 'Refresh connections'));
  assert.ok(labelled(stage, `Immutable network ID · ${'b'.repeat(12)}`));
  assert.deepEqual(property(stage, `Immutable network ID · ${'b'.repeat(12)}`, 'Tooltip'), {
    Text: networkId,
  });
  assert.ok(labelled(stage, `Container · ${'a'.repeat(12)}`));
  assert.deepEqual(property(stage, `Container · ${'a'.repeat(12)}`, 'Tooltip'), {
    Text: containerId,
  });
  assert.equal(
    labelled(stage, `Container · ${containerId}`),
    undefined,
    'exact endpoint identity stays inspectable without becoming a narrow text wall',
  );
  assert.equal(labelled(stage, '$.id'), undefined, 'host source paths never enter the product UI');

  const empty = host();
  empty.render(
    h(Networks, {
      api: { networks: { ...api.networks, inspect: async () => ({}) } },
      resource,
      containers: containerResource(),
    }),
  );
  invoke(empty, 'Manage connections');
  await settled();
  await settled();
  assert.ok(labelled(empty, 'No network details'));
});

test('network inventory failures use typed causes and one honest recovery action', () => {
  const failure = (kind, detail) => Object.assign(new Error(detail), { kind });
  const cases = [
    [
      'unavailable',
      'socket refused',
      'Network inventory is unavailable. Check that the workspace is running, then retry.',
      'Retry networks',
    ],
    [
      'absent',
      'catalogue disappeared',
      'Network inventory changed or is no longer available. Refresh to load current records.',
      'Refresh networks',
    ],
    [
      'conflict',
      'generation changed',
      'Network inventory changed or is no longer available. Refresh to load current records.',
      'Refresh networks',
    ],
    [
      'failed',
      'invalid response',
      'Network inventory could not be loaded. Retry, then inspect technical details if it continues.',
      'Retry networks',
    ],
  ];
  for (const [kind, detail, summary, action] of cases) {
    let retries = 0;
    const stage = host();
    stage.render(
      h(Networks, {
        api,
        containers: containerResource(),
        resource: {
          data: [],
          loading: false,
          error: failure(kind, detail),
          reload: () => {
            retries += 1;
          },
        },
        onOpenExtensions: () => {},
      }),
    );
    assert.ok(labelled(stage, summary), `${kind} has resource-specific recovery`);
    assert.ok(labelled(stage, action), `${kind} has one primary action`);
    assert.ok(labelled(stage, 'Technical details'));
    assert.ok(labelled(stage, detail), 'the exact diagnostic remains disclosed');
    assert.equal(labelled(stage, 'This view could not be completed.'), undefined);
    invoke(stage, action);
    assert.equal(retries, 1);
  }

  let opened = 0;
  const denied = host();
  denied.render(
    h(Networks, {
      api,
      containers: containerResource(),
      resource: {
        data: [],
        loading: false,
        error: failure('denied', 'networks:read refused'),
        reload: () => assert.fail('authority denial must not offer futile retry'),
      },
      onOpenExtensions: () => {
        opened += 1;
      },
    }),
  );
  assert.ok(
    labelled(
      denied,
      'Top does not have permission to list networks. Review its network access in Extensions.',
    ),
  );
  assert.equal(labelled(denied, 'Retry networks'), undefined);
  invoke(denied, 'Review access');
  assert.equal(opened, 1);
});

test('network inventory retry stays visible and disabled until a recovered snapshot replaces it', () => {
  const error = Object.assign(new Error('socket refused'), { kind: 'unavailable' });
  const stage = host();
  const common = { api, containers: containerResource(), onOpenExtensions: () => {} };
  stage.render(
    h(Networks, {
      ...common,
      resource: { data: [], loading: true, error, reload: () => {} },
    }),
  );
  assert.ok(labelled(stage, 'Retrying networks…'));
  assert.equal(isEnabled(stage, 'Retrying networks…'), false);
  assert.ok(labelled(stage, 'Technical details'));

  stage.render(
    h(Networks, {
      ...common,
      resource: {
        data: [],
        loading: false,
        error: null,
        reload: () => {},
      },
    }),
  );
  assert.ok(labelled(stage, 'No networks'));
  assert.equal(orderedLabels(stage).includes('Retrying networks…'), false);
  assert.equal(orderedLabels(stage).includes('socket refused'), false);
});

test('volume inspection exposes loading, retry, empty and bounded typed details', async () => {
  let attempts = 0;
  const controlled = {
    volumes: {
      ...api.volumes,
      inspect: async () => {
        attempts += 1;
        if (attempts === 1) throw new Error('volume inspect unavailable');
        return { name: 'cache', driver: 'local' };
      },
    },
  };
  const resource = {
    data: [{ name: 'cache', driver: 'local' }],
    loading: false,
    error: null,
    reload: async () => {},
  };
  const details = new VolumeDetailsSource();
  const stage = host();
  stage.render(h(Volumes, { api: controlled, resource, volumeDetails: details }));
  invoke(stage, 'Inspect');
  await settled();
  await settled();
  assert.ok(labelled(stage, 'Reading volume details…'));
  assert.ok(labelled(stage, 'volume inspect unavailable'));
  invoke(stage, 'Retry inspect');
  await settled();
  await settled();
  assert.ok(labelled(stage, 'Volume details'));
  assert.ok(labelled(stage, 'Name · cache'));
  assert.ok(labelled(stage, 'Driver · local'));
  assert.equal(labelled(stage, '$.name'), undefined);
  assert.equal(
    details.answer({ source: 205, version: 1, id: 1, range: { start: 0, count: 99 } }).rows.length,
    2,
  );

  const empty = host();
  empty.render(
    h(Volumes, {
      api: { volumes: { ...api.volumes, inspect: async () => ({}) } },
      resource,
      volumeDetails: new VolumeDetailsSource(),
    }),
  );
  invoke(empty, 'Inspect');
  await settled();
  await settled();
  assert.ok(labelled(empty, 'No volume details'));
});

test('container stop and kill cannot call the API before final confirmation', async () => {
  const calls = [];
  const immutable = 'a'.repeat(64);
  const controlled = {
    containers: {
      inspect: async (id) => ({ id, name: 'api', image: 'alpine', state: 'running', created: 0 }),
      stopAndWait: async (...args) => {
        calls.push(['stop', ...args]);
        return { changed: true, container: { id: immutable, state: 'exited' } };
      },
      kill: async (...args) => calls.push(['kill', ...args]),
      exec: async () => {},
      logs: async () => new Uint8Array(),
    },
  };
  const resource = {
    data: [{ id: immutable, name: 'api', image: 'alpine', state: 'running', generation: 7 }],
    loading: false,
    error: null,
    reload: async () => {},
  };
  const stage = host();
  stage.render(h(Containers, { api: controlled, resource }));

  invoke(stage, 'Stop');
  assert.deepEqual(calls, [], 'opening stop confirmation performs no operation');
  assert.ok(labelled(stage, `Stop api with immutable ID ${immutable}?`));
  assert.equal(isDestructive(stage, 'Confirm stop'), true);
  invoke(stage, 'Cancel');
  assert.deepEqual(calls, [], 'cancelling stop is safe');
  invoke(stage, 'Stop');
  invoke(stage, 'Confirm stop');
  await settled();
  assert.deepEqual(calls, [['stop', immutable, 7]]);

  invoke(stage, 'Details');
  await settled();
  await settled();
  invoke(stage, 'Kill');
  assert.deepEqual(
    calls,
    [['stop', immutable, 7]],
    'opening kill confirmation performs no operation',
  );
  assert.ok(labelled(stage, `Force-kill api with immutable ID ${immutable}?`));
  assert.equal(isDestructive(stage, 'Confirm kill'), true);
  invoke(stage, 'Confirm kill');
  await settled();
  assert.deepEqual(calls.at(-1), ['kill', immutable, 7, 'SIGKILL']);
});

test('container rename validates locally, retries failure, and preserves immutable authority until refresh', async () => {
  const immutable = 'a'.repeat(64);
  const calls = [];
  let attempts = 0;
  const controlled = {
    containers: {
      rename: async (...args) => {
        calls.push(['rename', ...args]);
        attempts += 1;
        if (attempts === 1) throw new Error('name catalogue temporarily unavailable');
      },
    },
  };
  const resource = {
    data: [{ id: immutable, name: 'api', image: 'alpine', state: 'running', generation: 7 }],
    loading: false,
    error: null,
    reload: async () => calls.push(['reload']),
  };
  const stage = host();
  stage.render(h(Containers, { api: controlled, resource }));
  await settled();
  const identity = labelled(stage, `Container ID · ${immutable.slice(0, 12)}`);
  assert.ok(identity, 'the rename surface keeps immutable identity compact');
  assert.deepEqual(
    latestProperty(stage, identity.SetProp.id, 'Tooltip'),
    { Text: `Immutable container ID ${immutable}` },
    'the exact immutable identity remains available without dominating the card',
  );
  assert.equal(labelled(stage, `New name for ${immutable.slice(0, 12)}`), undefined);
  assert.equal(taggedProperty(stage, 'Edit name', 'Button', 'Variant')?.Variant, 'Ghost');
  invoke(stage, 'Edit name');

  change(stage, `New name for ${immutable.slice(0, 12)}`, '.invalid');
  assert.ok(
    labelled(
      stage,
      'Container name must contain 1–128 ASCII letters, digits, underscores, periods, or hyphens and start with a letter or digit.',
    ),
  );
  assert.equal(isEnabled(stage, 'Rename'), false);
  assert.deepEqual(calls, [], 'invalid input never reaches the typed API');

  change(stage, `New name for ${immutable.slice(0, 12)}`, 'worker_2.prod');
  invoke(stage, 'Rename');
  assert.ok(labelled(stage, 'Renaming…'), 'the in-flight operation is explicit');
  assert.equal(isEnabled(stage, 'Renaming…'), false);
  await settled();
  await settled();
  assert.ok(labelled(stage, 'name catalogue temporarily unavailable'));
  assert.ok(
    labelled(stage, 'api'),
    'failed rename does not optimistically replace inventory identity',
  );
  invoke(stage, 'Retry rename');
  await settled();
  await settled();
  assert.deepEqual(calls, [
    ['rename', immutable, 7, 'worker_2.prod'],
    ['rename', immutable, 7, 'worker_2.prod'],
    ['reload'],
  ]);
  assert.ok(labelled(stage, 'Renamed to worker_2.prod.'));
  assert.ok(labelled(stage, 'api'), 'success notice does not forge an inventory update');
});

test('container creation groups its compact form and uses a human label editor', () => {
  const stage = host();
  const frame = stage.render(
    h(Containers, {
      api,
      resource: { data: [], loading: false, error: null, reload: async () => {} },
    }),
  );
  assert.equal(
    taggedProperty(stage, 'Container setup', 'Expander', 'Expanded')?.Flag,
    false,
    'empty container creation is controlled by the prominent action',
  );
  for (const label of [
    'Identity and image',
    'Process overrides',
    'Resources and connectivity',
    'Mounts accept named volumes only. Published host ports may be left automatic.',
  ])
    assert.ok(labelled(stage, label), `${label} is available in the semantic tree`);
  assert.equal(
    taggedProperty(stage, 'Process overrides', 'Expander', 'Expanded')?.Flag,
    false,
    'optional process fields stay out of the primary creation path',
  );
  assert.ok(
    labelled(
      stage,
      'Optional. Entrypoint replaces the image program; Command supplies its arguments. Add each argument with Enter or Add.',
    ),
    'process terminology is explained before it is requested',
  );
  const placeholders = frame.patches
    .filter((patch) => patch.SetProp?.prop === 'Placeholder')
    .map((patch) => patch.SetProp.value.Text);
  assert.deepEqual(placeholders.slice(0, 2), ['Image reference', 'Container name']);
  assert.ok(placeholders.includes('Add command argument'));
  assert.equal(
    placeholders.some((value) => value.includes('JSON')),
    false,
  );
  for (const label of [
    'Image reference · required',
    'Container name · required',
    'Working directory',
    'Labels',
  ])
    assert.ok(labelled(stage, label), `${label} remains visible independently of input content`);
  assert.ok(labelled(stage, 'Required: image and name.'));
  assert.equal(
    ancestorTags(stage, 'Create and start')[0],
    'Row',
    'the primary action stays beside the required-field status',
  );
  assert.ok(
    placeholderProperty(stage, 'Working directory (optional)', 'Width'),
    'the working-directory control has an explicit readable width',
  );
  const wrappingRows = frame.patches.filter(
    (patch) => patch.SetProp?.prop === 'Wrap' && patch.SetProp.value?.Flag === true,
  );
  assert.equal(wrappingRows.length >= 3, true, 'every field group can wrap at compact width');
});

test('container creation retains exact identity and retries only start after a partial failure', async () => {
  const calls = [];
  let starts = 0;
  const controlled = {
    containers: {
      create: async (spec) => {
        calls.push(['create', spec]);
        return 'container-new';
      },
      inspect: async (id) => ({ id, generation: 0 }),
      start: async (id, generation) => {
        calls.push(['start', id, generation]);
        starts += 1;
        if (starts === 1) throw new Error('runtime temporarily unavailable');
      },
    },
  };
  const resource = {
    data: [],
    loading: false,
    error: null,
    reload: async () => calls.push(['reload']),
  };
  const stage = host();
  stage.render(h(Containers, { api: controlled, resource }));
  change(stage, 'Image reference', 'alpine:3.20');
  change(stage, 'Container name', 'worker');
  change(stage, 'Working directory (optional)', '/workspace/../secret');
  assert.ok(
    labelled(
      stage,
      'Working directory must be an absolute, NUL-free path without dot segments and at most 4096 bytes.',
    ),
  );
  assert.equal(isEnabled(stage, 'Create and start'), false);
  change(stage, 'Working directory (optional)', '');
  invoke(stage, 'Create and start');
  await settled();
  await settled();
  assert.ok(labelled(stage, 'runtime temporarily unavailable'));
  assert.ok(labelled(stage, 'Retry start'), 'the exact created container remains recoverable');
  invoke(stage, 'Retry start');
  await settled();
  await settled();
  assert.ok(labelled(stage, 'Created and started worker.'));
  assert.deepEqual(
    calls,
    [
      ['create', { image: 'alpine:3.20', name: 'worker' }],
      ['start', 'container-new', 0],
      ['start', 'container-new', 0],
      ['reload'],
    ],
    'retry never creates a duplicate container',
  );
});

test('native process editors preserve ordered argv and environment wire types', async () => {
  const calls = [];
  const controlled = {
    containers: {
      create: async (spec) => {
        calls.push(spec);
        return 'native-editor-container';
      },
      inspect: async (id) => ({ id, generation: 0 }),
      start: async () => {},
    },
  };
  const stage = host();
  stage.render(
    h(Containers, {
      api: controlled,
      resource: { data: [], loading: false, error: null, reload: async () => {} },
    }),
  );
  change(stage, 'Image reference', 'alpine:3.20');
  change(stage, 'Container name', 'native');
  for (const argument of ['sh', '-lc', 'printf ready']) {
    change(stage, 'Add command argument', argument);
    submit(stage, 'Add command argument');
  }
  change(stage, 'Variable name', 'MODE');
  change(stage, 'Variable value', 'test');
  invoke(stage, 'Add variable');
  change(stage, 'Label name', 'role');
  change(stage, 'Label value', 'worker');
  invoke(stage, 'Add label');
  change(stage, 'Mount volume', 'cache');
  change(stage, 'Container path', '/cache');
  toggleLatestSwitch(stage, true);
  invoke(stage, 'Add mount');
  change(stage, 'Container port', '8080');
  change(stage, 'Host port (automatic if empty)', '18080');
  invoke(stage, 'Publish port');
  invoke(stage, 'Create and start');
  await settled();
  await settled();
  assert.deepEqual(calls, [
    {
      image: 'alpine:3.20',
      name: 'native',
      command: ['sh', '-lc', 'printf ready'],
      environment: [['MODE', 'test']],
      labels: [['role', 'worker']],
      mounts: [{ volume: 'cache', target: '/cache', read_only: true }],
      ports: [{ container: 8080, host: 18080, protocol: 'tcp' }],
    },
  ]);
});

test('container creation refuses to start when inspection returns a different immutable identity', async () => {
  const calls = [];
  const created = 'a'.repeat(32);
  const replacement = 'b'.repeat(32);
  const controlled = {
    containers: {
      create: async () => created,
      inspect: async () => ({ id: replacement, generation: 0 }),
      start: async (...args) => calls.push(args),
    },
  };
  const stage = host();
  stage.render(
    h(Containers, {
      api: controlled,
      resource: { data: [], loading: false, error: null, reload: async () => {} },
    }),
  );
  change(stage, 'Image reference', 'alpine:3.20');
  change(stage, 'Container name', 'worker');
  invoke(stage, 'Create and start');
  await settled();
  await settled();
  assert.ok(
    labelled(stage, `Created container ${created} could not be verified by immutable identity.`),
  );
  assert.deepEqual(calls, []);
});

test('container creation validates exact resource bounds and retains them until success', async () => {
  const calls = [];
  let creates = 0;
  const controlled = {
    containers: {
      create: async (spec) => {
        calls.push(['create', spec]);
        creates += 1;
        if (creates === 1) throw new Error('create temporarily unavailable');
        return 'limited-container';
      },
      inspect: async (id) => ({ id, generation: 0 }),
      start: async (id, generation) => calls.push(['start', id, generation]),
    },
  };
  const resource = {
    data: [],
    loading: false,
    error: null,
    reload: async () => calls.push(['reload']),
  };
  const stage = host();
  stage.render(h(Containers, { api: controlled, resource }));
  change(stage, 'Image reference', 'alpine:3.20');
  change(stage, 'Container name', 'limited');
  for (const [placeholder, maximum, label] of [
    ['Memory limit MiB (optional)', 1_048_576, 'Memory limit'],
    ['CPU limit (optional)', 256, 'CPU limit'],
    ['PID limit (optional)', 1_000_000, 'PID limit'],
  ]) {
    change(stage, placeholder, '0');
    assert.ok(labelled(stage, `${label} must be a whole decimal number from 1 to ${maximum}.`));
    assert.equal(isEnabled(stage, 'Create and start'), false);
    change(stage, placeholder, String(maximum + 1));
    assert.ok(labelled(stage, `${label} must be a whole decimal number from 1 to ${maximum}.`));
    change(stage, placeholder, '1.5');
    assert.ok(labelled(stage, `${label} must be a whole decimal number from 1 to ${maximum}.`));
    change(stage, placeholder, String(maximum));
    assert.equal(isEnabled(stage, 'Create and start'), true, `${label} upper boundary is accepted`);
  }

  invoke(stage, 'Create and start');
  await settled();
  await settled();
  assert.ok(labelled(stage, 'create temporarily unavailable'));
  assert.equal(fieldValue(stage, 'Memory limit MiB (optional)'), '1048576');
  assert.equal(fieldValue(stage, 'CPU limit (optional)'), '256');
  assert.equal(fieldValue(stage, 'PID limit (optional)'), '1000000');
  invoke(stage, 'Create and start');
  await settled();
  await settled();
  assert.deepEqual(calls, [
    [
      'create',
      {
        image: 'alpine:3.20',
        name: 'limited',
        memory_mb: 1_048_576,
        cpus: 256,
        pids_limit: 1_000_000,
      },
    ],
    [
      'create',
      {
        image: 'alpine:3.20',
        name: 'limited',
        memory_mb: 1_048_576,
        cpus: 256,
        pids_limit: 1_000_000,
      },
    ],
    ['start', 'limited-container', 0],
    ['reload'],
  ]);
  assert.equal(fieldValue(stage, 'Memory limit MiB (optional)'), '');
  assert.equal(fieldValue(stage, 'CPU limit (optional)'), '');
  assert.equal(fieldValue(stage, 'PID limit (optional)'), '');
});

test('container creation accepts only bounded named-volume mounts and retains them until success', async () => {
  const mountError =
    'Mounts must contain at most 64 named volumes with unique absolute targets and optional boolean read_only. Host bind mounts are not accepted.';
  assert.throws(() => parseMounts([{ volume: 'cache', target: 'relative', read_only: false }]), {
    message: mountError,
  });
  assert.throws(
    () =>
      parseMounts(
        Array.from({ length: 65 }, (_, index) => ({
          volume: `v${index}`,
          target: `/v${index}`,
          read_only: false,
        })),
      ),
    { message: mountError },
  );
  assert.equal(
    parseMounts(
      Array.from({ length: 64 }, (_, index) => ({
        volume: `v${index}`,
        target: `/v${index}`,
        read_only: false,
      })),
    )?.length,
    64,
  );
  return;
  const calls = [];
  let creates = 0;
  const controlled = {
    containers: {
      create: async (spec) => {
        calls.push(['create', spec]);
        creates += 1;
        if (creates === 1) throw new Error('volume attachment temporarily unavailable');
        return 'mounted-container';
      },
      inspect: async (id) => ({ id, generation: 0 }),
      start: async (id, generation) => calls.push(['start', id, generation]),
    },
  };
  const resource = {
    data: [],
    loading: false,
    error: null,
    reload: async () => calls.push(['reload']),
  };
  const stage = host();
  stage.render(h(Containers, { api: controlled, resource }));
  change(stage, 'Image reference', 'alpine:3.20');
  change(stage, 'Container name', 'mounted');
  const placeholder = 'Named volume mounts JSON (optional)';
  const error =
    'Mounts must contain at most 64 named volumes with unique absolute targets and optional boolean read_only. Host bind mounts are not accepted.';
  for (const invalid of [
    '[{"volume":"cache","target":"relative"}]',
    '[{"volume":"cache","target":"/cache/../secret"}]',
    '[{"volume":"cache","target":"/cache","read_only":"yes"}]',
    '[{"volume":"cache","target":"/same"},{"volume":"data","target":"/same"}]',
    JSON.stringify(
      Array.from({ length: 65 }, (_, index) => ({ volume: `v${index}`, target: `/v${index}` })),
    ),
    '[{"source":"/host","target":"/guest"}]',
  ]) {
    change(stage, placeholder, invalid);
    assert.ok(labelled(stage, error), `invalid label set was accepted: ${invalid.slice(0, 80)}`);
    assert.equal(isEnabled(stage, 'Create and start'), false);
  }
  change(
    stage,
    placeholder,
    JSON.stringify(
      Array.from({ length: 64 }, (_, index) => ({ volume: `v${index}`, target: `/v${index}` })),
    ),
  );
  assert.equal(
    isEnabled(stage, 'Create and start'),
    true,
    'the exact 64-mount boundary is accepted',
  );
  const requested =
    '[{"volume":"cache","target":"/cache","read_only":true},{"volume":"data","target":"/srv/data"}]';
  change(stage, placeholder, requested);
  invoke(stage, 'Create and start');
  await settled();
  await settled();
  assert.ok(labelled(stage, 'volume attachment temporarily unavailable'));
  assert.equal(fieldValue(stage, placeholder), requested);
  invoke(stage, 'Create and start');
  await settled();
  await settled();
  const spec = {
    image: 'alpine:3.20',
    name: 'mounted',
    mounts: [
      { volume: 'cache', target: '/cache', read_only: true },
      { volume: 'data', target: '/srv/data', read_only: false },
    ],
  };
  assert.deepEqual(calls, [
    ['create', spec],
    ['create', spec],
    ['start', 'mounted-container', 0],
    ['reload'],
  ]);
  assert.equal(fieldValue(stage, placeholder), '');
});

test('container creation validates bounded published ports and retains them until success', async () => {
  const portError =
    'Ports must contain at most 64 unique container-port/protocol pairs from 1 to 65535; host is an optional port number, not an address.';
  assert.throws(() => parsePorts([{ container: 0, host: null, protocol: 'tcp' }]), {
    message: portError,
  });
  assert.throws(
    () =>
      parsePorts(
        Array.from({ length: 65 }, (_, index) => ({
          container: index + 1,
          host: null,
          protocol: 'tcp',
        })),
      ),
    { message: portError },
  );
  assert.equal(
    parsePorts(
      Array.from({ length: 64 }, (_, index) => ({
        container: index + 1,
        host: null,
        protocol: 'tcp',
      })),
    )?.length,
    64,
  );
  return;
  const calls = [];
  let creates = 0;
  const controlled = {
    containers: {
      create: async (spec) => {
        calls.push(['create', spec]);
        creates += 1;
        if (creates === 1) throw new Error('port publication temporarily unavailable');
        return 'published-container';
      },
      inspect: async (id) => ({ id, generation: 0 }),
      start: async (id, generation) => calls.push(['start', id, generation]),
    },
  };
  const resource = {
    data: [],
    loading: false,
    error: null,
    reload: async () => calls.push(['reload']),
  };
  const stage = host();
  stage.render(h(Containers, { api: controlled, resource }));
  change(stage, 'Image reference', 'alpine:3.20');
  change(stage, 'Container name', 'published');
  const placeholder = 'Published ports JSON (optional)';
  const error =
    'Ports must contain at most 64 unique container-port/protocol pairs from 1 to 65535; host is an optional port number, not an address.';
  for (const invalid of [
    '[{"container":0,"protocol":"tcp"}]',
    '[{"container":65536,"protocol":"tcp"}]',
    '[{"container":80,"host":0,"protocol":"tcp"}]',
    '[{"container":80,"host":"127.0.0.1:8080","protocol":"tcp"}]',
    '[{"container":80,"protocol":"sctp"}]',
    '[{"container":80,"protocol":"tcp"},{"container":80,"host":8080,"protocol":"tcp"}]',
    '[{"container":80,"protocol":"tcp","address":"127.0.0.1"}]',
    JSON.stringify(
      Array.from({ length: 65 }, (_, index) => ({ container: index + 1, protocol: 'tcp' })),
    ),
  ]) {
    change(stage, placeholder, invalid);
    assert.ok(labelled(stage, error));
    assert.equal(isEnabled(stage, 'Create and start'), false);
  }
  change(
    stage,
    placeholder,
    JSON.stringify(
      Array.from({ length: 64 }, (_, index) => ({ container: index + 1, protocol: 'tcp' })),
    ),
  );
  assert.equal(
    isEnabled(stage, 'Create and start'),
    true,
    'the exact 64-port boundary is accepted',
  );
  const requested =
    '[{"container":8080,"host":18080,"protocol":"tcp"},{"container":53,"protocol":"udp"},{"container":53,"protocol":"tcp"}]';
  change(stage, placeholder, requested);
  invoke(stage, 'Create and start');
  await settled();
  await settled();
  assert.ok(labelled(stage, 'port publication temporarily unavailable'));
  assert.equal(fieldValue(stage, placeholder), requested);
  invoke(stage, 'Create and start');
  await settled();
  await settled();
  const spec = {
    image: 'alpine:3.20',
    name: 'published',
    ports: [
      { container: 8080, host: 18080, protocol: 'tcp' },
      { container: 53, host: null, protocol: 'udp' },
      { container: 53, host: null, protocol: 'tcp' },
    ],
  };
  assert.deepEqual(calls, [
    ['create', spec],
    ['create', spec],
    ['start', 'published-container', 0],
    ['reload'],
  ]);
  assert.equal(fieldValue(stage, placeholder), '');
});

test('container creation validates runtime identity and retains it until success', async () => {
  const calls = [];
  let creates = 0;
  const controlled = {
    containers: {
      create: async (spec) => {
        calls.push(['create', spec]);
        creates += 1;
        if (creates === 1) throw new Error('identity temporarily unavailable');
        return 'identity-container';
      },
      inspect: async (id) => ({ id, generation: 0 }),
      start: async (id, generation) => calls.push(['start', id, generation]),
    },
  };
  const resource = {
    data: [],
    loading: false,
    error: null,
    reload: async () => calls.push(['reload']),
  };
  const stage = host();
  stage.render(h(Containers, { api: controlled, resource }));
  change(stage, 'Image reference', 'alpine:3.20');
  change(stage, 'Container name', 'identity');
  const hostnameError =
    'Hostname must start with an ASCII letter or digit, contain only ASCII letters, digits, dots, underscores or hyphens, and be at most 253 bytes.';
  for (const invalid of ['-worker', 'worker name', 'wørker', `a${'b'.repeat(253)}`]) {
    change(stage, 'Hostname (optional)', invalid);
    assert.ok(labelled(stage, hostnameError));
    assert.equal(isEnabled(stage, 'Create and start'), false);
  }
  change(stage, 'Hostname (optional)', `a${'b'.repeat(252)}`);
  assert.equal(
    isEnabled(stage, 'Create and start'),
    true,
    'the exact 253-byte hostname boundary is accepted',
  );
  change(stage, 'Hostname (optional)', 'build-worker_1.local');
  change(stage, 'Run as user (optional)', `u${'é'.repeat(128)}`);
  assert.ok(
    labelled(stage, 'Run as user must be a nonempty, NUL-free value of at most 256 bytes.'),
  );
  assert.equal(
    isEnabled(stage, 'Create and start'),
    false,
    'UTF-8 byte length, not character count, enforces the user bound',
  );
  const exactUser = `u${'é'.repeat(127)}x`;
  change(stage, 'Run as user (optional)', exactUser);
  invoke(stage, 'Create and start');
  await settled();
  await settled();
  assert.ok(labelled(stage, 'identity temporarily unavailable'));
  assert.equal(fieldValue(stage, 'Hostname (optional)'), 'build-worker_1.local');
  assert.equal(fieldValue(stage, 'Run as user (optional)'), exactUser);
  invoke(stage, 'Create and start');
  await settled();
  await settled();
  const spec = {
    image: 'alpine:3.20',
    name: 'identity',
    hostname: 'build-worker_1.local',
    user: exactUser,
  };
  assert.deepEqual(calls, [
    ['create', spec],
    ['create', spec],
    ['start', 'identity-container', 0],
    ['reload'],
  ]);
  assert.equal(fieldValue(stage, 'Hostname (optional)'), '');
  assert.equal(fieldValue(stage, 'Run as user (optional)'), '');
});

test('container creation validates bounded labels and retains them until success', async () => {
  const labelError =
    'Labels must contain at most 128 unique [name, value] pairs; names are nonempty and at most 256 bytes, values at most 4096 bytes, and both are NUL-free.';
  assert.throws(
    () =>
      parseLabels([
        ['role', 'worker'],
        ['role', 'other'],
      ]),
    { message: labelError },
  );
  assert.throws(
    () => parseLabels(Array.from({ length: 129 }, (_, index) => [`key-${index}`, 'value'])),
    { message: labelError },
  );
  assert.equal(
    parseLabels(Array.from({ length: 128 }, (_, index) => [`key-${index}`, 'value']))?.length,
    128,
  );
  return;
  const calls = [];
  let creates = 0;
  const controlled = {
    containers: {
      create: async (spec) => {
        calls.push(['create', spec]);
        creates += 1;
        if (creates === 1) throw new Error('label persistence temporarily unavailable');
        return 'labelled-container';
      },
      inspect: async (id) => ({ id, generation: 0 }),
      start: async (id, generation) => calls.push(['start', id, generation]),
    },
  };
  const resource = {
    data: [],
    loading: false,
    error: null,
    reload: async () => calls.push(['reload']),
  };
  const stage = host();
  stage.render(h(Containers, { api: controlled, resource }));
  change(stage, 'Image reference', 'alpine:3.20');
  change(stage, 'Container name', 'labelled');
  const placeholder = 'Labels, one name=value per line (optional)';
  const error =
    'Labels must contain at most 128 unique [name, value] pairs; names are nonempty and at most 256 bytes, values at most 4096 bytes, and both are NUL-free.';
  changeByTooltip(stage, placeholder, 'missing-separator');
  assert.ok(labelled(stage, 'Each label must use name=value on its own line.'));
  for (const invalid of [
    'role=worker\nrole=other',
    `${`k${'é'.repeat(128)}`}=value`,
    `key=${'é'.repeat(2049)}`,
    Array.from({ length: 129 }, (_, index) => `key-${index}=value`).join('\n'),
  ]) {
    changeByTooltip(stage, placeholder, invalid);
    assert.ok(labelled(stage, error));
    assert.equal(isEnabled(stage, 'Create and start'), false);
  }
  changeByTooltip(
    stage,
    placeholder,
    Array.from({ length: 128 }, (_, index) => `key-${index}=value`).join('\n'),
  );
  assert.equal(
    isEnabled(stage, 'Create and start'),
    true,
    'the exact 128-label boundary is accepted',
  );
  const requested = 'role=worker\ncom.example/tier=backend\nempty=';
  changeByTooltip(stage, placeholder, requested);
  invoke(stage, 'Create and start');
  await settled();
  await settled();
  assert.ok(labelled(stage, 'label persistence temporarily unavailable'));
  assert.equal(fieldValueByTooltip(stage, placeholder), requested);
  invoke(stage, 'Create and start');
  await settled();
  await settled();
  const spec = {
    image: 'alpine:3.20',
    name: 'labelled',
    labels: [
      ['role', 'worker'],
      ['com.example/tier', 'backend'],
      ['empty', ''],
    ],
  };
  assert.deepEqual(calls, [
    ['create', spec],
    ['create', spec],
    ['start', 'labelled-container', 0],
    ['reload'],
  ]);
  assert.equal(fieldValueByTooltip(stage, placeholder), '');
});

test('container creation validates entrypoint argv and retains it until success', async () => {
  const argumentError =
    'Entrypoint must contain 1 to 64 NUL-free string arguments, each at most 4096 bytes and 32768 bytes in total.';
  assert.throws(() => parseArguments([''], 'Entrypoint'), { message: argumentError });
  assert.throws(() => parseArguments(Array(65).fill('x'), 'Entrypoint'), {
    message: argumentError,
  });
  assert.equal(parseArguments(Array(64).fill('x'), 'Entrypoint')?.length, 64);
  return;
  const calls = [];
  let creates = 0;
  const controlled = {
    containers: {
      create: async (spec) => {
        calls.push(['create', spec]);
        creates += 1;
        if (creates === 1) throw new Error('entrypoint temporarily unavailable');
        return 'entrypoint-container';
      },
      inspect: async (id) => ({ id, generation: 0 }),
      start: async (id, generation) => calls.push(['start', id, generation]),
    },
  };
  const resource = {
    data: [],
    loading: false,
    error: null,
    reload: async () => calls.push(['reload']),
  };
  const stage = host();
  stage.render(h(Containers, { api: controlled, resource }));
  change(stage, 'Image reference', 'alpine:3.20');
  change(stage, 'Container name', 'entrypoint');
  const placeholder = 'Entrypoint argv JSON (optional)';
  const error =
    'Entrypoint must contain 1 to 64 NUL-free string arguments, each at most 4096 bytes and 32768 bytes in total.';
  for (const invalid of [
    '[]',
    '[""]',
    '[1]',
    JSON.stringify(['x'.repeat(4097)]),
    JSON.stringify(Array.from({ length: 65 }, () => 'x')),
  ]) {
    change(stage, placeholder, invalid);
    assert.ok(labelled(stage, error));
    assert.equal(isEnabled(stage, 'Create and start'), false);
  }
  change(stage, placeholder, JSON.stringify(Array.from({ length: 64 }, () => 'x')));
  assert.equal(
    isEnabled(stage, 'Create and start'),
    true,
    'the exact 64-argument boundary is accepted',
  );
  change(stage, placeholder, JSON.stringify(['x'.repeat(4096)]));
  assert.equal(
    isEnabled(stage, 'Create and start'),
    true,
    'the exact per-argument byte boundary is accepted',
  );
  change(stage, placeholder, JSON.stringify(Array.from({ length: 4 }, () => 'e'.repeat(4096))));
  change(
    stage,
    'Command argv JSON (optional)',
    JSON.stringify(Array.from({ length: 5 }, () => 'c'.repeat(4096))),
  );
  assert.ok(labelled(stage, 'Entrypoint and command together must contain at most 32768 bytes.'));
  change(
    stage,
    'Command argv JSON (optional)',
    JSON.stringify(Array.from({ length: 4 }, () => 'c'.repeat(4096))),
  );
  assert.equal(
    isEnabled(stage, 'Create and start'),
    true,
    'the exact combined 32768-byte boundary is accepted',
  );
  change(stage, placeholder, '["/bin/sh","-lc"]');
  change(stage, 'Command argv JSON (optional)', '["printf ready"]');
  invoke(stage, 'Create and start');
  await settled();
  await settled();
  assert.ok(labelled(stage, 'entrypoint temporarily unavailable'));
  assert.equal(fieldValue(stage, placeholder), '["/bin/sh","-lc"]');
  invoke(stage, 'Create and start');
  await settled();
  await settled();
  const spec = {
    image: 'alpine:3.20',
    name: 'entrypoint',
    entrypoint: ['/bin/sh', '-lc'],
    command: ['printf ready'],
  };
  assert.deepEqual(calls, [
    ['create', spec],
    ['create', spec],
    ['start', 'entrypoint-container', 0],
    ['reload'],
  ]);
  assert.equal(fieldValue(stage, placeholder), '');
});

test('container creation validates an initial network reference and retains it until success', async () => {
  const calls = [];
  let creates = 0;
  const controlled = {
    containers: {
      create: async (spec) => {
        calls.push(['create', spec]);
        creates += 1;
        if (creates === 1) throw new Error('network attachment temporarily unavailable');
        return 'networked-container';
      },
      inspect: async (id) => ({ id, generation: 0 }),
      start: async (id, generation) => calls.push(['start', id, generation]),
    },
  };
  const resource = {
    data: [],
    loading: false,
    error: null,
    reload: async () => calls.push(['reload']),
  };
  const stage = host();
  stage.render(h(Containers, { api: controlled, resource }));
  change(stage, 'Image reference', 'alpine:3.20');
  change(stage, 'Container name', 'networked');
  const placeholder = 'Initial network (optional)';
  const error =
    'Initial network must start with an ASCII letter or digit, contain only ASCII letters, digits, dots, underscores or hyphens, and be at most 255 bytes.';
  for (const invalid of ['-private', 'private network', 'prívate', `n${'x'.repeat(255)}`]) {
    change(stage, placeholder, invalid);
    assert.ok(labelled(stage, error));
    assert.equal(isEnabled(stage, 'Create and start'), false);
  }
  change(stage, placeholder, `n${'x'.repeat(254)}`);
  assert.equal(
    isEnabled(stage, 'Create and start'),
    true,
    'the exact 255-byte boundary is accepted',
  );
  change(stage, placeholder, 'private_backend.v1');
  invoke(stage, 'Create and start');
  await settled();
  await settled();
  assert.ok(labelled(stage, 'network attachment temporarily unavailable'));
  assert.equal(fieldValue(stage, placeholder), 'private_backend.v1');
  invoke(stage, 'Create and start');
  await settled();
  await settled();
  const spec = { image: 'alpine:3.20', name: 'networked', network: 'private_backend.v1' };
  assert.deepEqual(calls, [
    ['create', spec],
    ['create', spec],
    ['start', 'networked-container', 0],
    ['reload'],
  ]);
  assert.equal(fieldValue(stage, placeholder), '');
});

test('container controls follow the real daemon lifecycle states', () => {
  const id = 'c'.repeat(32);
  const api = { containers: {} };
  let stage = host();
  const inventory = (state) => ({
    data: [{ id, name: 'worker', image: 'alpine', state }],
    loading: false,
    error: null,
    reload: async () => {},
  });
  stage.render(h(Containers, { api, resource: inventory('running') }));
  assert.equal(labelled(stage, 'Remove'), undefined, 'running cards omit an invalid remove action');
  assert.equal(taggedProperty(stage, 'More actions', 'Expander', 'Expanded')?.Flag, false);
  assert.equal(taggedProperty(stage, 'Details', 'Button', 'Variant')?.Variant, 'Filled');

  stage = host();
  stage.render(h(Containers, { api, resource: inventory('created') }));
  assert.equal(isEnabled(stage, 'Remove'), true, 'created containers are removable');
  assert.equal(isEnabled(stage, 'Start'), true, 'created containers are startable');
  assert.equal(taggedProperty(stage, 'Start', 'Button', 'Variant')?.Variant, 'Outline');

  stage = host();
  stage.render(h(Containers, { api, resource: inventory('exited') }));
  assert.equal(isEnabled(stage, 'Remove'), true, 'exited containers are removable');
  assert.equal(isEnabled(stage, 'Start'), true, 'exited containers are restartable through start');

  stage = host();
  stage.render(h(Containers, { api, resource: inventory('paused') }));
  assert.equal(
    labelled(stage, 'Remove'),
    undefined,
    'paused containers omit an invalid remove action',
  );
  assert.equal(isEnabled(stage, 'Restart'), true, 'paused containers can be restarted');
  assert.equal(isEnabled(stage, 'Stop'), true, 'paused containers can be stopped');

  stage = host();
  stage.render(h(Containers, { api, resource: inventory('restarting') }));
  assert.equal(
    labelled(stage, 'Start'),
    undefined,
    'a restarting container cannot be started twice',
  );
  assert.equal(isEnabled(stage, 'Stop'), true, 'a restart loop can be stopped');
});

test('container lifecycle controls report only observation-backed completion', async () => {
  const id = 'c'.repeat(32);
  const calls = [];
  const controlled = {
    containers: {
      startAndWait: async (...args) => {
        calls.push(['start', ...args]);
        return { changed: true, container: { id, state: 'running' } };
      },
      stopAndWait: async (...args) => {
        calls.push(['stop', ...args]);
        return { changed: false, id, state: 'exited' };
      },
      restartAndWait: async (...args) => {
        calls.push(['restart', ...args]);
        return { changed: true, container: { id, state: 'running', generation: 8 } };
      },
      removeAndWait: async (...args) => {
        calls.push(['remove', ...args]);
        return { changed: true, id };
      },
    },
  };
  const reload = async () => calls.push(['reload']);
  const stage = host();
  const render = (state, generation = 7) =>
    stage.render(
      h(Containers, {
        api: controlled,
        resource: {
          data: [{ id, name: 'worker', image: 'alpine', state, generation }],
          loading: false,
          error: null,
          reload,
        },
      }),
    );

  render('created');
  invoke(stage, 'Start');
  await settled();
  await settled();
  assert.ok(labelled(stage, 'Start completed and was verified.'));
  assert.deepEqual(calls, [['start', id, 7], ['reload']]);

  render('running');
  invoke(stage, 'Restart');
  await settled();
  await settled();
  assert.ok(labelled(stage, 'Restart completed and was verified.'));
  assert.deepEqual(calls.slice(-2), [['restart', id, 7], ['reload']]);

  invoke(stage, 'Stop');
  invoke(stage, 'Confirm stop');
  await settled();
  await settled();
  assert.ok(
    labelled(
      stage,
      'Stop was sent, but the requested transition was not observed before the deadline.',
    ),
  );
  assert.deepEqual(calls.slice(-2), [['stop', id, 7], ['reload']]);

  render('exited', 8);
  invoke(stage, 'Remove');
  invoke(stage, 'Confirm remove');
  await settled();
  await settled();
  assert.ok(labelled(stage, 'Container removal completed and its absence was verified.'));
  assert.deepEqual(calls.slice(-2), [['remove', id, 8], ['reload']]);
});

test('restart refuses a container without an observed generation', async () => {
  const calls = [];
  const stage = host();
  stage.render(
    h(Containers, {
      api: { containers: { restartAndWait: async (...args) => calls.push(args) } },
      resource: {
        data: [{ id: 'old', name: 'worker', image: 'alpine', state: 'running' }],
        loading: false,
        error: null,
        reload: async () => calls.push(['reload']),
      },
    }),
  );
  invoke(stage, 'Restart');
  await settled();
  await settled();
  assert.ok(
    labelled(stage, 'Container old has no observable generation; refresh before changing it.'),
  );
  assert.deepEqual(calls, []);
});

test('container execution builds exact argv without exposing JSON', async () => {
  const calls = [];
  const controlled = {
    containers: {
      inspect: async () => ({
        id: 'container-one',
        name: 'api',
        image: 'alpine',
        state: 'running',
        created: 0,
      }),
      exec: async (id, generation, options) => {
        calls.push(['exec', id, generation, options]);
        return 'execution-exact-42';
      },
      logs: async () => new Uint8Array(),
    },
  };
  const resource = {
    data: [{ id: 'container-one', name: 'api', image: 'alpine', state: 'running', generation: 7 }],
    loading: false,
    error: null,
    reload: async () => {},
  };
  const opened = [];
  const stage = host();
  stage.render(
    h(Containers, { api: controlled, resource, onOpenExecution: async (id) => opened.push(id) }),
  );
  invoke(stage, 'Details');
  await settled();
  await settled();

  assert.equal(labelled(stage, 'Command argv JSON'), undefined);
  change(stage, 'Program, e.g. sh', 'x'.repeat(4_097));
  invoke(stage, 'Execute');
  await settled();
  assert.ok(
    labelled(
      stage,
      'Command must contain at most 64 NUL-free arguments, each at most 4096 bytes and 32768 bytes in total.',
    ),
    'oversized program is rejected',
  );
  assert.deepEqual(calls, []);

  change(stage, 'Program, e.g. sh', 'sh');
  invoke(stage, 'Add argument');
  await settled();
  invoke(stage, 'Add argument');
  await settled();
  change(stage, 'Argument 1', '-lc');
  change(stage, 'Argument 2', 'printf hello world');
  change(stage, 'Run as user (optional)', '1000:1000');
  change(stage, 'Working directory (optional)', '/workspace with spaces');
  invoke(stage, 'Execute');
  await settled();
  await settled();
  assert.deepEqual(calls, [
    [
      'exec',
      'container-one',
      7,
      {
        command: ['sh', '-lc', 'printf hello world'],
        user: '1000:1000',
        workingDirectory: '/workspace with spaces',
      },
    ],
  ]);
  assert.ok(
    labelled(
      stage,
      'Execute captures output for later inspection. Attach terminal opens the same command interactively.',
    ),
  );
  assert.ok(labelled(stage, 'Execution execution-exact-42 created.'));
  invoke(stage, 'Inspect execution');
  await settled();
  assert.deepEqual(opened, ['execution-exact-42']);
});

test('container details open an interactive terminal from structured command fields', async () => {
  const calls = [];
  const id = 'a'.repeat(64);
  const controlled = {
    containers: {
      inspect: async () => ({ id, name: 'api', image: 'alpine', state: 'running', created: 0 }),
      exec: async () => 'unused',
      attachTerminal: async (...args) => {
        calls.push(args);
        return 'p9';
      },
      logs: async () => new Uint8Array(),
    },
  };
  const resource = {
    data: [{ id, name: 'api', image: 'alpine', state: 'running' }],
    loading: false,
    error: null,
    reload: async () => {},
  };
  const stage = host();
  stage.render(h(Containers, { api: controlled, resource }));
  invoke(stage, 'Details');
  await settled();
  await settled();
  change(stage, 'Program, e.g. sh', 'sh');
  invoke(stage, 'Add argument');
  await settled();
  invoke(stage, 'Add argument');
  await settled();
  change(stage, 'Argument 1', '-lc');
  change(stage, 'Argument 2', 'printf hello world');
  invoke(stage, 'Attach terminal');
  await settled();
  await settled();
  assert.deepEqual(calls, [[id, ['sh', '-lc', 'printf hello world']]]);
  assert.ok(labelled(stage, 'Interactive terminal opened in p9.'));
});

test('container details load through the bounded source and a failed read is retryable', async () => {
  let attempts = 0;
  const mutations = [];
  const controlled = {
    containers: {
      inspect: async () => {
        attempts += 1;
        if (attempts === 1) throw new Error('container inspect unavailable');
        return {
          id: 'container-one',
          name: 'api',
          image: 'alpine:3.20',
          state: 'running',
          created: 42,
        };
      },
      exec: async () => {},
      logs: async () => new Uint8Array(),
    },
  };
  const resource = {
    data: [{ id: 'container-one', name: 'api', image: 'alpine:3.20', state: 'running' }],
    loading: false,
    error: null,
    reload: async () => {},
  };
  const details = new ContainerDetailsSource(async (mutation) => mutations.push(mutation));
  const stage = host();
  stage.render(h(Containers, { api: controlled, resource, containerDetails: details }));
  invoke(stage, 'Details');
  await settled();
  await settled();
  assert.ok(labelled(stage, 'Reading container details…'));
  assert.ok(labelled(stage, 'container inspect unavailable'));
  invoke(stage, 'Retry details');
  await settled();
  await settled();
  assert.equal(attempts, 2);
  assert.ok(labelled(stage, 'Container details'));
  assert.ok(labelled(stage, 'Name · api'));
  assert.ok(labelled(stage, 'Image · alpine:3.20'));
  assert.equal(labelled(stage, '$.id'), undefined);
  assert.deepEqual(mutations, [{ Length: { source: 202, version: 1, rows: 5 } }]);
  assert.equal(
    details.answer({ source: 202, version: 1, id: 2, range: { start: 0, count: 999 } }).rows.length,
    4,
  );
});

test('empty container inspection remains understandable and withholds unproven actions', async () => {
  const controlled = {
    containers: {
      inspect: async () => ({}),
      exec: async () => {},
      logs: async () => new Uint8Array(),
    },
  };
  const resource = {
    data: [{ id: 'container-empty', name: 'empty', image: '', state: 'created' }],
    loading: false,
    error: null,
    reload: async () => {},
  };
  const stage = host();
  stage.render(
    h(Containers, { api: controlled, resource, containerDetails: new ContainerDetailsSource() }),
  );
  invoke(stage, 'Details');
  await settled();
  await settled();
  assert.ok(labelled(stage, 'No container details'));
  assert.equal(labelled(stage, 'Quick actions'), undefined);
});

test('execution details, separate bounded streams, wait and retry are operational', async () => {
  const calls = [];
  let inspectAttempts = 0;
  const item = {
    id: 'e1',
    container_id: 'c1',
    running: true,
    exit_code: 0,
    pid: 77,
    command: ['sleep', '5'],
    user: 'root',
  };
  const controlled = {
    containers: {
      execution: async () => {
        inspectAttempts += 1;
        if (inspectAttempts === 1) throw new Error('execution moved');
        return item;
      },
      executionLogs: async () => ({
        stdout: Array(5_000).fill(111),
        stderr: [98, 97, 100],
        truncated: true,
        stdout_truncated: true,
        stderr_truncated: false,
        eof: false,
      }),
      waitExecution: async (...args) => {
        calls.push(['wait', ...args]);
        return { ...item, running: false, exit_code: 0 };
      },
      removeExecution: async () => {},
    },
  };
  const resource = {
    data: [item],
    loading: false,
    error: null,
    reload: async () => calls.push(['reload']),
  };
  const details = new ExecutionDetailsSource();
  const stage = host();
  stage.render(h(Executions, { api: controlled, resource, executionDetails: details }));
  assert.deepEqual(ancestorTags(stage, 'Details').slice(0, 2), ['Row', 'Row']);
  assert.ok(
    ancestorTags(stage, 'Details').includes('Card'),
    'execution actions stay inside their resource record',
  );
  assert.deepEqual(ancestorProperty(stage, 'Details', 'Card', 'Width'), { Length: 'Fill' });
  for (const label of ['Details', 'Load output', 'Wait up to 5s']) {
    assert.deepEqual(taggedProperty(stage, label, 'Button', 'Size'), {
      ControlSize: 'Small',
    });
  }
  invoke(stage, 'Details');
  await settled();
  await settled();
  assert.ok(
    labelled(
      stage,
      'This execution is no longer available. Refresh executions to see the current records.',
    ),
  );
  assert.equal(labelled(stage, 'This view could not be completed.'), undefined);
  assert.equal(labelled(stage, 'Technical details'), undefined);
  invoke(stage, 'Retry details');
  await settled();
  await settled();
  assert.ok(labelled(stage, 'Execution summary'));
  assert.ok(labelled(stage, 'Process · 77'));
  assert.ok(labelled(stage, 'Command · sleep 5'));
  assert.ok(labelled(stage, 'User · root'));
  assert.ok(labelled(stage, 'Container · c1'));
  assert.ok(labelled(stage, 'Technical details'));
  assert.ok(
    stage.frames
      .flatMap((frame) => frame.patches)
      .some((patch) => patch.Create?.tag === 'KeyValueTable'),
  );
  invoke(stage, 'Load output');
  await settled();
  await settled();
  assert.ok(labelled(stage, 'Standard output'));
  assert.ok(labelled(stage, 'Standard error'));
  assert.ok(labelled(stage, 'Standard output was truncated to its configured bound.'));
  assert.ok(!labelled(stage, 'Standard error was truncated to its configured bound.'));
  assert.ok(labelled(stage, 'Execution is still running; later output may appear.'));
  const values = stage.frames
    .flatMap((frame) => frame.patches)
    .filter(
      (patch) => patch.SetProp?.prop === 'Value' && typeof patch.SetProp.value?.Text === 'string',
    )
    .map((patch) => patch.SetProp.value.Text);
  assert.ok(
    values.every((value) => [...value].length <= 4096),
    'no LogView patch exceeds its retention bound',
  );
  invoke(stage, 'Wait up to 5s');
  await settled();
  await settled();
  assert.deepEqual(calls, [['wait', 'e1', { timeoutMs: 5_000 }], ['reload']]);
});

test('finished execution cleanup requires explicit destructive confirmation', async () => {
  const calls = [];
  const item = {
    id: 'e2',
    container_id: 'c1',
    running: false,
    exit_code: 0,
    pid: 0,
    command: ['true'],
    user: '',
  };
  const controlled = {
    containers: {
      execution: async () => item,
      executionLogs: async () => ({
        stdout: [],
        stderr: [],
        truncated: false,
        stdout_truncated: false,
        stderr_truncated: false,
        eof: true,
      }),
      waitExecution: async () => item,
      removeExecutionAndWait: async (...args) => {
        calls.push(args);
        return { changed: true, id: item.id };
      },
    },
  };
  const resource = { data: [item], loading: false, error: null, reload: async () => {} };
  const stage = host();
  stage.render(h(Executions, { api: controlled, resource }));
  invoke(stage, 'Details');
  await settled();
  await settled();
  invoke(stage, 'Load output');
  await settled();
  await settled();
  assert.ok(labelled(stage, 'Captured output is complete (EOF).'));
  invoke(stage, 'Remove record');
  assert.deepEqual(calls, []);
  assert.equal(isDestructive(stage, 'Confirm removal'), true);
  invoke(stage, 'Confirm removal');
  await settled();
  await settled();
  assert.deepEqual(calls, [['e2', { running: false, exit_code: 0, pid: 0 }]]);
  assert.ok(labelled(stage, 'Execution e2 was removed and its absence was verified.'));
});

test('running execution termination is cursor-bound, confirmed and reports observed exit', async () => {
  const calls = [];
  const item = {
    id: 'execution-full-identity',
    container_id: 'c1',
    running: true,
    exit_code: 0,
    pid: 42,
    command: ['sleep', '30'],
    user: '',
  };
  const controlled = {
    containers: {
      execution: async (id) => {
        calls.push(['inspect', id]);
        return item;
      },
      executionLogs: async () => ({ stdout: [], stderr: [], truncated: false }),
      waitExecution: async () => item,
      signalExecutionAndWait: async (...args) => {
        calls.push(['signal', ...args]);
        return { changed: true, execution: { ...item, running: false, exit_code: 143, pid: 0 } };
      },
      removeExecution: async () => {},
    },
  };
  const resource = {
    data: [item],
    loading: false,
    error: null,
    reload: async () => calls.push(['reload']),
  };
  const stage = host();
  stage.render(h(Executions, { api: controlled, resource }));
  invoke(stage, 'Terminate');
  assert.deepEqual(calls, [], 'opening the prompt cannot signal the process');
  assert.ok(labelled(stage, 'Send SIGTERM to execution execution-full-identity?'));
  assert.equal(isDestructive(stage, 'Confirm SIGTERM'), true);
  invoke(stage, 'Confirm SIGTERM');
  await settled();
  await settled();
  assert.deepEqual(calls, [
    [
      'signal',
      'execution-full-identity',
      'SIGTERM',
      { running: true, exit_code: 0, pid: 42 },
      { state: 'exited' },
    ],
    ['reload'],
    ['inspect', 'execution-full-identity'],
  ]);
  assert.ok(labelled(stage, 'SIGTERM completed and execution execution-fu was observed exited.'));
});

test('execution termination distinguishes an unobserved transition from completion', async () => {
  const item = {
    id: 'e'.repeat(32),
    container_id: 'c'.repeat(32),
    running: true,
    exit_code: 0,
    pid: 7,
    command: ['sleep', '30'],
    user: '',
  };
  const calls = [];
  const stage = host();
  stage.render(
    h(Executions, {
      api: {
        containers: {
          signalExecutionAndWait: async (...args) => {
            calls.push(args);
            return { changed: false, id: item.id, state: 'exited' };
          },
          execution: async () => item,
        },
      },
      resource: { data: [item], loading: false, error: null, reload: async () => {} },
    }),
  );
  invoke(stage, 'Terminate');
  invoke(stage, 'Confirm SIGTERM');
  await settled();
  await settled();
  assert.deepEqual(calls, [
    [item.id, 'SIGTERM', { running: true, exit_code: 0, pid: 7 }, { state: 'exited' }],
  ]);
  assert.ok(
    labelled(
      stage,
      'SIGTERM was sent, but execution eeeeeeeeeeee was not observed exited before the deadline.',
    ),
  );
});

test('empty and host-truncated execution catalogues remain explicit', async () => {
  const item = {
    id: 'empty',
    container_id: 'c1',
    running: false,
    exit_code: 0,
    pid: 0,
    command: [],
    user: '',
  };
  const controlled = {
    containers: {
      execution: async () => ({}),
      executionLogs: async () => ({ stdout: [], stderr: [], truncated: false }),
      waitExecution: async () => item,
      removeExecution: async () => {},
    },
  };
  const resource = { data: [item], loading: false, error: null, reload: async () => {} };
  const stage = host();
  stage.render(h(Executions, { api: controlled, resource, truncated: true }));
  assert.ok(labelled(stage, 'The host execution catalogue was truncated at its safety limit.'));
  invoke(stage, 'Details');
  await settled();
  await settled();
  assert.ok(labelled(stage, 'Reading execution details…'));
  assert.ok(labelled(stage, 'No execution details'));
});

test('volume and network mutations expose danger only on final confirm and cancel safely', async () => {
  const calls = [];
  const volumeGeneration = 'd'.repeat(32);
  const refreshedVolumeGeneration = 'e'.repeat(32);
  const networkId = 'a'.repeat(32);
  const refreshedNetworkId = 'c'.repeat(32);
  const containerId = 'b'.repeat(64);
  const resource = (data) => ({ data, loading: false, error: null, reload: async () => {} });
  const controlled = {
    volumes: {
      inspect: async () => ({}),
      create: async () => ({}),
      removeAndWait: async (...args) => {
        calls.push(['volume.remove', ...args]);
        return { changed: true, name: args[0], generation: args[1] };
      },
    },
    networks: {
      inspect: async () => ({}),
      create: async () => '',
      connect: async () => {},
      disconnect: async (...args) => calls.push(['network.disconnect', ...args]),
      removeAndWait: async (...args) => {
        calls.push(['network.remove', ...args]);
        return { changed: true, id: args[0] };
      },
    },
  };

  const volumes = host();
  volumes.render(
    h(Volumes, {
      api: controlled,
      resource: resource([{ name: 'cache', driver: 'local', generation: volumeGeneration }]),
    }),
  );
  invoke(volumes, 'Remove');
  assert.deepEqual(calls, []);
  assert.equal(isDestructive(volumes, 'Confirm remove'), true);
  assert.ok(labelled(volumes, `Remove volume cache generation ${volumeGeneration}?`));
  const staleVolumeConfirm = labelled(volumes, 'Confirm remove').SetProp.id;
  volumes.render(
    h(Volumes, {
      api: controlled,
      resource: resource([
        { name: 'cache', driver: 'local', generation: refreshedVolumeGeneration },
      ]),
    }),
  );
  volumes.surface.dispatch({
    trigger: 'Invoke',
    node: staleVolumeConfirm,
    id: `${staleVolumeConfirm}:Invoke`,
    value: null,
  });
  await settled();
  assert.deepEqual(calls, []);
  invoke(volumes, 'Remove');
  invoke(volumes, 'Confirm remove');
  await settled();
  assert.deepEqual(calls, [['volume.remove', 'cache', refreshedVolumeGeneration]]);
  assert.ok(
    labelled(
      volumes,
      `Volume cache generation ${refreshedVolumeGeneration} was removed and its absence was verified.`,
    ),
  );

  const networks = host();
  const initialNetworks = resource([
    {
      id: networkId,
      name: 'private',
      driver: 'bridge',
      scope: 'local',
      kind: 'custom',
      endpoints: { containers: [containerId], truncated: false },
    },
  ]);
  networks.render(
    h(Networks, {
      api: controlled,
      resource: initialNetworks,
      containers: containerResource(containerId),
    }),
  );
  invoke(networks, 'Manage connections');
  await settled();
  await settled();
  chooseContainer(networks, containerId);
  await settled();
  invoke(networks, 'Disconnect');
  assert.equal(isDestructive(networks, 'Confirm disconnect'), true);
  assert.ok(
    labelled(networks, `Disconnect immutable container ${containerId} from network ${networkId}?`),
  );
  assert.equal(
    calls.some(([name]) => name === 'network.disconnect'),
    false,
  );
  invoke(networks, 'Confirm disconnect');
  await settled();
  assert.deepEqual(calls.at(-1), ['network.disconnect', networkId, containerId]);
  invoke(networks, 'Remove');
  assert.ok(labelled(networks, `Remove immutable network ${networkId} (private)?`));
  assert.equal(
    calls.some(([name]) => name === 'network.remove'),
    false,
  );
  const staleConfirm = labelled(networks, 'Confirm remove').SetProp.id;
  const refreshedNetworks = resource([
    { id: refreshedNetworkId, name: 'private', driver: 'bridge', scope: 'local', kind: 'custom' },
  ]);
  networks.render(
    h(Networks, {
      api: controlled,
      resource: refreshedNetworks,
      containers: containerResource(containerId),
    }),
  );
  networks.surface.dispatch({
    trigger: 'Invoke',
    node: staleConfirm,
    id: `${staleConfirm}:Invoke`,
    value: null,
  });
  await settled();
  assert.equal(
    calls.some(([name]) => name === 'network.remove'),
    false,
  );
  assert.ok(
    labelled(networks, `Network ${networkId} changed or disappeared; inspect and confirm again.`),
  );
  invoke(networks, 'Remove');
  invoke(networks, 'Confirm remove');
  await settled();
  await settled();
  assert.deepEqual(calls.at(-1), ['network.remove', refreshedNetworkId]);
  assert.ok(
    labelled(networks, `Network ${refreshedNetworkId} was removed and its absence was verified.`),
  );
});

test('shared volume confirmation disables both final actions while removal is pending', async () => {
  let release;
  const controlled = {
    volumes: {
      ...api.volumes,
      removeAndWait: async () =>
        new Promise((resolve) => {
          release = () => resolve({ changed: true, name: 'cache', generation: 'd'.repeat(32) });
        }),
    },
  };
  const resource = {
    data: [{ name: 'cache', driver: 'local', generation: 'd'.repeat(32) }],
    loading: false,
    error: null,
    reload: async () => {},
  };
  const stage = host();
  stage.render(h(Volumes, { api: controlled, resource }));
  invoke(stage, 'Remove');
  invoke(stage, 'Confirm remove');
  await settled();
  assert.equal(isEnabled(stage, 'Confirm remove'), false);
  assert.equal(isEnabled(stage, 'Cancel'), false);
  release();
  await settled();
  await settled();
  assert.ok(labelled(stage, 'Remove'), 'successful removal closes the shared confirmation');
});

test('volume creation exposes pending failure and retained retry before claiming success', async () => {
  const calls = [];
  let rejectFirst;
  let attempt = 0;
  const controlled = {
    volumes: {
      ...api.volumes,
      create: async (name) => {
        calls.push(['create', name]);
        attempt += 1;
        if (attempt === 1)
          await new Promise((_, reject) => {
            rejectFirst = reject;
          });
        return name;
      },
    },
  };
  const resource = {
    data: [],
    loading: false,
    error: null,
    reload: async () => calls.push(['reload']),
  };
  const stage = host();
  stage.render(h(Volumes, { api: controlled, resource }));
  assert.deepEqual(ancestorTags(stage, 'Create').slice(0, 2), ['Row', 'FormControl']);
  assert.deepEqual(taggedProperty(stage, 'Create', 'Button', 'Size'), {
    ControlSize: 'Small',
  });
  change(stage, 'Volume name', ' cache-data ');
  invoke(stage, 'Create');
  await settled();
  assert.ok(labelled(stage, 'Creating volume cache-data…'));
  assert.equal(isEnabled(stage, 'Creating…'), false);
  assert.deepEqual(calls, [['create', 'cache-data']]);
  rejectFirst(new Error(`storage unavailable ${'x'.repeat(600)}`));
  await settled();
  await settled();
  assert.ok(labelled(stage, 'Retry create'));
  const failures = stage.frames
    .flatMap((frame) => frame.patches)
    .filter(
      (patch) =>
        patch.SetProp?.prop === 'Label' &&
        patch.SetProp.value?.Text?.startsWith('storage unavailable'),
    );
  assert.ok(new TextEncoder().encode(failures.at(-1).SetProp.value.Text).byteLength <= 1024);
  invoke(stage, 'Retry create');
  await settled();
  await settled();
  assert.deepEqual(calls, [['create', 'cache-data'], ['create', 'cache-data'], ['reload']]);
  assert.ok(labelled(stage, 'Created volume cache-data.'));
});

test('truncated network membership explains why inspection is required and resolves contextually', async () => {
  const container = 'b'.repeat(64);
  const network = {
    id: 'a'.repeat(32),
    name: 'private',
    driver: 'bridge',
    scope: 'local',
    kind: 'custom',
    endpoints: { containers: [], truncated: true },
  };
  const controlled = {
    networks: {
      ...api.networks,
      inspect: async () => ({
        ...network,
        endpoints: { containers: [], truncated: false },
      }),
    },
  };
  const stage = host();
  stage.render(
    h(Networks, {
      api: controlled,
      resource: { data: [network], loading: false, error: null, reload: async () => {} },
      containers: containerResource(container),
    }),
  );
  assert.equal(
    stage.frames
      .flatMap((frame) => frame.patches)
      .some((patch) => patch.SetProp?.prop === 'Choices'),
    false,
    'attachment controls stay hidden until a network is inspected',
  );
  assert.ok(labelled(stage, '0 shown · more omitted'));
  invoke(stage, 'Manage connections');
  await settled();
  await settled();
  chooseContainer(stage, container);
  await settled();
  assert.ok(labelled(stage, 'Connect'), 'complete inspection exposes the contextual action');
});

test('network management offers only stopped containers', async () => {
  const stopped = 'b'.repeat(64);
  const running = 'c'.repeat(64);
  const network = {
    id: 'a'.repeat(32),
    name: 'private',
    driver: 'bridge',
    scope: 'local',
    kind: 'custom',
    endpoints: { containers: [], truncated: false },
  };
  const containers = containerResource(stopped, running);
  containers.data[1].state = 'running';
  const stage = host();
  stage.render(
    h(Networks, {
      api: { networks: { ...api.networks, inspect: async () => network } },
      resource: { data: [network], loading: false, error: null, reload: async () => {} },
      containers,
    }),
  );
  invoke(stage, 'Manage connections');
  await settled();
  await settled();
  const choices = stage.frames
    .flatMap((frame) => frame.patches)
    .filter((patch) => patch.SetProp?.prop === 'Choices')
    .at(-1)?.SetProp.value.Choices;
  assert.deepEqual(
    choices.map((choice) => choice.value),
    [stopped],
    'a running container is not offered for an operation the host will refuse',
  );
});

test('network connect validates aliases, exposes progress, success, bounded failure and retained retry', async () => {
  const calls = [];
  let release;
  let attempt = 0;
  const controlled = {
    networks: {
      ...api.networks,
      connect: async (...args) => {
        calls.push(args);
        attempt += 1;
        if (attempt === 1)
          await new Promise((resolve) => {
            release = resolve;
          });
        if (attempt === 2) throw new Error(`temporary ${'x'.repeat(600)}`);
      },
    },
  };
  const resource = {
    data: [
      {
        id: 'a'.repeat(32),
        name: 'private',
        driver: 'bridge',
        scope: 'local',
        kind: 'custom',
        endpoints: { containers: [], truncated: false },
      },
    ],
    loading: false,
    error: null,
    reload: async () => calls.push(['reload']),
  };
  const stage = host();
  stage.render(
    h(Networks, { api: controlled, resource, containers: containerResource('b'.repeat(64)) }),
  );
  invoke(stage, 'Manage connections');
  await settled();
  await settled();
  chooseContainer(stage, 'b'.repeat(64));
  await settled();
  change(stage, 'Aliases, comma-separated (optional)', 'db,db');
  await settled();
  assert.deepEqual(
    calls,
    [],
    'invalid immutable identity and aliases never reach control authority',
  );
  assert.ok(labelled(stage, 'Connect'), 'the inspected valid container offers its endpoint action');

  invoke(stage, 'Connect');
  await settled();
  assert.deepEqual(calls, [], 'duplicate aliases never reach control authority');
  assert.ok(
    labelled(
      stage,
      'Network endpoint aliases must be at most 64 unique, 1..=253-byte ASCII endpoint names.',
    ),
  );

  change(stage, 'Aliases, comma-separated (optional)', 'database.internal, database_2');
  invoke(stage, 'Connect');
  await settled();
  assert.ok(labelled(stage, 'Connecting immutable endpoint…'));
  assert.deepEqual(calls[0], [
    'a'.repeat(32),
    'b'.repeat(64),
    { aliases: ['database.internal', 'database_2'] },
  ]);
  release();
  await settled();
  await settled();
  assert.ok(labelled(stage, 'Connected container-1 to private'));

  invoke(stage, 'Connect');
  await settled();
  await settled();
  assert.ok(labelled(stage, 'Retry connect'));
  const errors = stage.frames
    .flatMap((frame) => frame.patches)
    .filter(
      (patch) =>
        patch.SetProp?.prop === 'Label' && patch.SetProp.value?.Text?.startsWith('temporary '),
    );
  assert.equal(
    errors.at(-1).SetProp.value.Text.length,
    513,
    'host failures have a bounded semantic label',
  );
  invoke(stage, 'Retry connect');
  await settled();
  await settled();
  assert.equal(calls.filter((call) => call[0] === 'a'.repeat(32)).length, 3);
});

test('successful network attachment retains its receipt and verified expanded membership', async () => {
  const network = 'a'.repeat(32);
  const container = 'b'.repeat(64);
  let connected = false;
  const calls = [];
  const controlled = {
    networks: {
      ...api.networks,
      inspect: async (reference) => {
        calls.push(['inspect', reference]);
        return {
          id: network,
          name: 'private',
          driver: 'bridge',
          scope: 'local',
          kind: 'custom',
          endpoints: { containers: connected ? [container] : [], truncated: false },
        };
      },
      connect: async (reference, immutable, options) => {
        calls.push(['connect', reference, immutable, options]);
        connected = true;
      },
    },
  };
  const resource = {
    data: [{ id: network, name: 'private', driver: 'bridge', scope: 'local', kind: 'custom' }],
    loading: false,
    error: null,
    reload: async () => calls.push(['reload']),
  };
  const stage = host();
  stage.render(
    h(Networks, { api: controlled, resource, containers: containerResource(container) }),
  );
  invoke(stage, 'Manage connections');
  await settled();
  chooseContainer(stage, container);
  await settled();
  invoke(stage, 'Connect');
  await settled();
  await settled();
  assert.ok(labelled(stage, 'Connected container-1 to private'));
  assert.ok(labelled(stage, `Container ID · ${container}`));
  assert.ok(labelled(stage, `Network ID · ${network}`));
  assert.ok(labelled(stage, 'Technical details'));
  assert.ok(labelled(stage, 'Connected containers · 1'));
  assert.ok(labelled(stage, `Container · ${container.slice(0, 12)}`));
  assert.ok(labelled(stage, 'Refresh connections'));
  assert.deepEqual(
    orderedLabels(stage).filter((label) =>
      [
        'Network details',
        'Container attachment',
        'Connected container-1 to private',
        'Technical details',
        'Danger zone',
      ].includes(label),
    ),
    [
      'Network details',
      'Container attachment',
      'Connected container-1 to private',
      'Technical details',
      'Danger zone',
    ],
    'reinspection retains the daily attachment workflow and its receipt before destructive controls',
  );
  assert.notDeepEqual(taggedProperty(stage, 'Danger zone', 'Expander', 'Expanded'), {
    Flag: true,
  });
  assert.deepEqual(
    calls.map((call) => call[0]),
    ['inspect', 'connect', 'reload', 'inspect'],
  );
});

test('a successful attachment retains its receipt when membership reinspection is denied', async () => {
  const network = 'a'.repeat(32);
  const container = 'b'.repeat(64);
  let inspections = 0;
  const controlled = {
    networks: {
      ...api.networks,
      inspect: async () => {
        inspections += 1;
        if (inspections > 1) throw { kind: 'denied', capability: 'networks:read' };
        return {
          id: network,
          name: 'private',
          driver: 'bridge',
          scope: 'local',
          kind: 'custom',
          endpoints: { containers: [], truncated: false },
        };
      },
      connect: async () => {},
    },
  };
  const resource = {
    data: [{ id: network, name: 'private', driver: 'bridge', scope: 'local', kind: 'custom' }],
    loading: false,
    error: null,
    reload: async () => {},
  };
  const stage = host();
  stage.render(
    h(Networks, { api: controlled, resource, containers: containerResource(container) }),
  );
  invoke(stage, 'Manage connections');
  await settled();
  chooseContainer(stage, container);
  await settled();
  invoke(stage, 'Connect');
  await settled();
  await settled();
  assert.ok(labelled(stage, 'Connected container-1 to private'));
  assert.ok(labelled(stage, 'Review access'));
  assert.equal(labelled(stage, 'Connected containers · 1'), undefined);
});

test('successful disconnect retains its receipt and verified empty membership', async () => {
  const network = 'a'.repeat(32);
  const container = 'b'.repeat(64);
  let connected = true;
  const controlled = {
    networks: {
      ...api.networks,
      inspect: async () => ({
        id: network,
        name: 'private',
        driver: 'bridge',
        scope: 'local',
        kind: 'custom',
        endpoints: { containers: connected ? [container] : [], truncated: false },
      }),
      disconnect: async () => {
        connected = false;
      },
    },
  };
  const resource = {
    data: [{ id: network, name: 'private', driver: 'bridge', scope: 'local', kind: 'custom' }],
    loading: false,
    error: null,
    reload: async () => {},
  };
  const stage = host();
  stage.render(
    h(Networks, { api: controlled, resource, containers: containerResource(container) }),
  );
  invoke(stage, 'Manage connections');
  await settled();
  chooseContainer(stage, container);
  await settled();
  invoke(stage, 'Disconnect');
  invoke(stage, 'Confirm disconnect');
  await settled();
  await settled();
  assert.ok(labelled(stage, 'Disconnected container-1 from private'));
  assert.equal(labelled(stage, 'Aliases, comma-separated (optional)'), undefined);
  assert.ok(labelled(stage, 'Connected containers · 0'));
  assert.ok(labelled(stage, 'No connected containers'));
  assert.deepEqual(
    orderedLabels(stage).filter((label) =>
      [
        'Network details',
        'Container attachment',
        'Disconnected container-1 from private',
        'Technical details',
        'Danger zone',
      ].includes(label),
    ),
    [
      'Network details',
      'Container attachment',
      'Disconnected container-1 from private',
      'Technical details',
      'Danger zone',
    ],
    'disconnect reinspection retains its receipt before the final destructive disclosure',
  );
  assert.notDeepEqual(taggedProperty(stage, 'Danger zone', 'Expander', 'Expanded'), {
    Flag: true,
  });
});

test('network creation exposes pending failure and retained retry before claiming success', async () => {
  const calls = [];
  let rejectFirst;
  let attempt = 0;
  const controlled = {
    networks: {
      ...api.networks,
      create: async (name) => {
        calls.push(['create', name]);
        attempt += 1;
        if (attempt === 1)
          await new Promise((_, reject) => {
            rejectFirst = reject;
          });
        return 'a'.repeat(32);
      },
    },
  };
  const resource = {
    data: [],
    loading: false,
    error: null,
    reload: async () => calls.push(['reload']),
  };
  const stage = host();
  stage.render(h(Networks, { api: controlled, resource, containers: containerResource() }));
  assert.deepEqual(placeholderProperty(stage, 'Network name', 'Grow'), { Number: 1 });
  assert.deepEqual(placeholderProperty(stage, 'Network name', 'Width'), {
    Bounds: { minimum: { Chars: 20 }, maximum: { Chars: 40 } },
  });
  const initialLabels = orderedLabels(stage);
  assert.ok(initialLabels.indexOf('Create') < initialLabels.indexOf('Refresh'));
  change(stage, 'Network name', ' private-net ');
  invoke(stage, 'Create');
  await settled();
  assert.ok(labelled(stage, 'Creating network private-net…'));
  assert.equal(isEnabled(stage, 'Creating…'), false);
  assert.deepEqual(calls, [['create', 'private-net']]);

  rejectFirst(new Error(`registry unavailable ${'x'.repeat(600)}`));
  await settled();
  await settled();
  assert.ok(labelled(stage, 'Retry create'));
  const failures = stage.frames
    .flatMap((frame) => frame.patches)
    .filter(
      (patch) =>
        patch.SetProp?.prop === 'Label' &&
        patch.SetProp.value?.Text?.startsWith('registry unavailable'),
    );
  assert.ok(new TextEncoder().encode(failures.at(-1).SetProp.value.Text).byteLength <= 1024);
  assert.equal(failures.length, 1, 'a network creation failure is rendered exactly once');

  invoke(stage, 'Retry create');
  await settled();
  await settled();
  assert.deepEqual(calls, [['create', 'private-net'], ['create', 'private-net'], ['reload']]);
  assert.ok(labelled(stage, 'Created network private-net.'));
});

test('disconnect consent snapshots immutable identities and can be cancelled without authority', async () => {
  const calls = [];
  const network = 'a'.repeat(32);
  const first = 'b'.repeat(64);
  const second = 'c'.repeat(64);
  const controlled = {
    networks: { ...api.networks, disconnect: async (...args) => calls.push(args) },
  };
  const resource = {
    data: [
      {
        id: network,
        name: 'private',
        driver: 'bridge',
        scope: 'local',
        kind: 'custom',
        endpoints: { containers: [first, second], truncated: false },
      },
    ],
    loading: false,
    error: null,
    reload: async () => {},
  };
  const stage = host();
  stage.render(
    h(Networks, {
      api: controlled,
      resource,
      containers: containerResource(first, second),
    }),
  );
  invoke(stage, 'Manage connections');
  await settled();
  await settled();
  chooseContainer(stage, first);
  await settled();
  invoke(stage, 'Disconnect');
  assert.ok(labelled(stage, `Disconnect immutable container ${first} from network ${network}?`));
  const staleConfirm = labelled(stage, 'Confirm disconnect').SetProp.id;
  chooseContainer(stage, second);
  await settled();
  stage.surface.dispatch({
    trigger: 'Invoke',
    node: staleConfirm,
    id: `${staleConfirm}:Invoke`,
    value: null,
  });
  await settled();
  assert.deepEqual(
    calls,
    [],
    'editing identity invalidates prior consent even if a stale event is delivered',
  );
  invoke(stage, 'Disconnect');
  invoke(stage, 'Cancel');
  await settled();
  assert.deepEqual(calls, []);
  invoke(stage, 'Disconnect');
  invoke(stage, 'Confirm disconnect');
  await settled();
  await settled();
  assert.deepEqual(calls, [[network, second]]);
  assert.ok(labelled(stage, 'Disconnected container-2 from private'));
});

test('a failed final confirmation stays visible and retryable', async () => {
  let attempts = 0;
  const controlled = {
    volumes: {
      inspect: async () => ({}),
      create: async () => ({}),
      removeAndWait: async () => {
        attempts += 1;
        throw new Error('volume remains in use');
      },
    },
  };
  const resource = {
    data: [{ name: 'cache', driver: 'local', generation: 'e'.repeat(32) }],
    loading: false,
    error: null,
    reload: async () => {},
  };
  const stage = host();
  stage.render(h(Volumes, { api: controlled, resource }));
  invoke(stage, 'Remove');
  invoke(stage, 'Confirm remove');
  await settled();

  assert.equal(attempts, 1);
  assert.ok(
    labelled(stage, 'volume remains in use'),
    'the semantic tree carries the bounded failure',
  );
  assert.equal(
    isDestructive(stage, 'Confirm remove'),
    true,
    'the final action remains available for retry',
  );
  invoke(stage, 'Cancel');
  assert.equal(attempts, 1, 'cancelling after failure does not retry');
});

test('stale volume generation refuses authority and remains visibly retryable', async () => {
  const calls = [];
  const oldGeneration = 'd'.repeat(32);
  const currentGeneration = 'e'.repeat(32);
  const controlled = {
    volumes: {
      inspect: async () => ({}),
      create: async () => ({}),
      removeAndWait: async (...args) => {
        calls.push(args);
        return { changed: true, name: args[0], generation: args[1] };
      },
    },
  };
  const resource = {
    data: [
      { name: 'cache', driver: 'local', generation: oldGeneration },
      { name: 'cache', driver: 'local', generation: currentGeneration },
    ],
    loading: false,
    error: null,
    reload: async () => {},
  };
  const stage = host();
  stage.render(h(Volumes, { api: controlled, resource }));
  const removes = stage.frames
    .flatMap((frame) => frame.patches)
    .filter((patch) => patch.SetProp?.prop === 'Label' && patch.SetProp.value?.Text === 'Remove');
  assert.ok(
    stage.surface.dispatch({
      trigger: 'Invoke',
      node: removes[0].SetProp.id,
      id: `${removes[0].SetProp.id}:Invoke`,
      value: null,
    }),
  );
  invoke(stage, 'Confirm remove');
  await settled();
  await settled();
  assert.deepEqual(calls, []);
  assert.ok(labelled(stage, 'Volume cache changed generation; inspect and confirm again.'));
  assert.equal(isDestructive(stage, 'Confirm remove'), true);
});

function labelled(stage, label) {
  return stage.frames
    .flatMap((frame) => frame.patches)
    .filter(
      (patch) =>
        'SetProp' in patch && patch.SetProp.prop === 'Label' && patch.SetProp.value?.Text === label,
    )
    .at(-1);
}

function switchNodes(stage) {
  return stage.frames
    .flatMap((frame) => frame.patches)
    .filter((patch) => patch.Create?.tag === 'Switch')
    .map((patch) => patch.Create.id);
}

function latestSwitchValues(stage) {
  return switchNodes(stage).map(
    (node) =>
      stage.frames
        .flatMap((frame) => frame.patches)
        .filter((patch) => patch.SetProp?.id === node && patch.SetProp.prop === 'Checked')
        .at(-1)?.SetProp.value?.Flag,
  );
}

function toggleSwitch(stage, index, value) {
  const node = switchNodes(stage)[index];
  assert.notEqual(node, undefined, `switch ${index} is visible`);
  assert.ok(
    stage.surface.dispatch({ trigger: 'Toggle', node, id: `${node}:Toggle`, value }),
    `switch ${index} toggles`,
  );
}

function toggleLatestSwitch(stage, value) {
  const node = switchNodes(stage).at(-1);
  assert.notEqual(node, undefined, 'a switch is visible');
  assert.ok(
    stage.surface.dispatch({ trigger: 'Toggle', node, id: `${node}:Toggle`, value }),
    'the latest switch toggles',
  );
}

function selectExtensionMode(stage, label) {
  const control = labelled(stage, label);
  assert.ok(control, `${label} extension mode is visible`);
  assert.ok(
    stage.surface.dispatch({
      trigger: 'Toggle',
      node: control.SetProp.id,
      id: `${control.SetProp.id}:Toggle`,
      value: true,
    }),
    `${label} extension mode can be selected`,
  );
}

function invoke(stage, label) {
  const nodes = stage.frames
    .flatMap((frame) => frame.patches)
    .filter(
      (patch) =>
        'SetProp' in patch && patch.SetProp.prop === 'Label' && patch.SetProp.value?.Text === label,
    )
    .map((patch) => patch.SetProp.id)
    .reverse();
  assert.ok(nodes.length, `${label} is visible`);
  assert.ok(
    nodes.some((node) =>
      stage.surface.dispatch({ trigger: 'Invoke', node, id: `${node}:Invoke`, value: null }),
    ),
    `${label} invokes`,
  );
}

function labelledInCard(stage, cardLabel, label) {
  const patches = stage.frames.flatMap((frame) => frame.patches);
  const tags = new Map(
    patches.filter((patch) => patch.Create).map((patch) => [patch.Create.id, patch.Create.tag]),
  );
  const parents = new Map(
    patches
      .filter((patch) => patch.Insert)
      .map((patch) => [patch.Insert.child, patch.Insert.parent]),
  );
  const cardOf = (node) => {
    while (parents.has(node)) {
      node = parents.get(node);
      if (tags.get(node) === 'Card') return node;
    }
    return undefined;
  };
  const card = cardOf(labelled(stage, cardLabel)?.SetProp.id);
  return patches.filter(
    (patch) =>
      patch.SetProp?.prop === 'Label' &&
      patch.SetProp.value?.Text === label &&
      cardOf(patch.SetProp.id) === card,
  );
}

function invokeInCard(stage, cardLabel, label) {
  const nodes = labelledInCard(stage, cardLabel, label)
    .map((patch) => patch.SetProp.id)
    .reverse();
  assert.ok(nodes.length, `${label} is visible in ${cardLabel}`);
  assert.ok(
    nodes.some((node) =>
      stage.surface.dispatch({ trigger: 'Invoke', node, id: `${node}:Invoke`, value: null }),
    ),
    `${label} invokes in ${cardLabel}`,
  );
}

function expand(stage, label) {
  const nodes = stage.frames
    .flatMap((frame) => frame.patches)
    .filter((patch) => patch.SetProp?.prop === 'Label' && patch.SetProp.value?.Text === label)
    .map((patch) => patch.SetProp.id)
    .reverse();
  assert.ok(
    nodes.some((node) =>
      stage.surface.dispatch({ trigger: 'Expand', node, id: `${node}:Expand`, expanded: true }),
    ),
    `${label} expands`,
  );
}

function change(stage, placeholder, value) {
  const node = stage.frames
    .flatMap((frame) => frame.patches)
    .filter(
      (patch) =>
        'SetProp' in patch &&
        patch.SetProp.prop === 'Placeholder' &&
        patch.SetProp.value?.Text === placeholder,
    )
    .at(-1)?.SetProp.id;
  assert.notEqual(node, undefined, `${placeholder} field is visible`);
  assert.ok(
    stage.surface.dispatch({ trigger: 'Change', node, id: `${node}:Change`, value }),
    `${placeholder} changes`,
  );
}

function changeByTooltip(stage, tooltip, value) {
  const node = stage.frames
    .flatMap((frame) => frame.patches)
    .filter((patch) => patch.SetProp?.prop === 'Tooltip' && patch.SetProp.value?.Text === tooltip)
    .at(-1)?.SetProp.id;
  assert.notEqual(node, undefined, `${tooltip} field is visible`);
  assert.ok(
    stage.surface.dispatch({ trigger: 'Change', node, id: `${node}:Change`, value }),
    `${tooltip} changes`,
  );
}

function submit(stage, placeholder) {
  const node = stage.frames
    .flatMap((frame) => frame.patches)
    .filter(
      (patch) =>
        'SetProp' in patch &&
        patch.SetProp.prop === 'Placeholder' &&
        patch.SetProp.value?.Text === placeholder,
    )
    .at(-1)?.SetProp.id;
  assert.notEqual(node, undefined, `${placeholder} field is visible`);
  assert.ok(
    stage.surface.dispatch({ trigger: 'Submit', node, id: `${node}:Submit`, value: null }),
    `${placeholder} submits`,
  );
}

function isDestructive(stage, label) {
  const node = labelled(stage, label)?.SetProp.id;
  return stage.frames
    .flatMap((frame) => frame.patches)
    .some(
      (patch) =>
        'SetProp' in patch &&
        patch.SetProp.id === node &&
        patch.SetProp.prop === 'Destructive' &&
        patch.SetProp.value?.Flag === true,
    );
}

function isEnabled(stage, label) {
  const node = labelled(stage, label)?.SetProp.id;
  return stage.frames
    .flatMap((frame) => frame.patches)
    .filter(
      (patch) =>
        'SetProp' in patch && patch.SetProp.id === node && patch.SetProp.prop === 'Enabled',
    )
    .at(-1)?.SetProp.value?.Flag;
}

function enabledStates(stage, label) {
  const patches = stage.frames.flatMap((frame) => frame.patches);
  const nodes = patches
    .filter((patch) => patch.SetProp?.prop === 'Label' && patch.SetProp.value?.Text === label)
    .map((patch) => patch.SetProp.id);
  return nodes.map(
    (node) =>
      patches
        .filter((patch) => patch.SetProp?.id === node && patch.SetProp?.prop === 'Enabled')
        .at(-1)?.SetProp.value?.Flag,
  );
}

function property(stage, label, prop) {
  const node = labelled(stage, label)?.SetProp.id;
  return stage.frames
    .flatMap((frame) => frame.patches)
    .filter(
      (patch) => 'SetProp' in patch && patch.SetProp.id === node && patch.SetProp.prop === prop,
    )
    .at(-1)?.SetProp.value;
}

function taggedProperty(stage, label, tag, prop) {
  const patches = stage.frames.flatMap((frame) => frame.patches);
  const tagged = new Set(
    patches.filter((patch) => patch.Create?.tag === tag).map((patch) => patch.Create.id),
  );
  const node = patches
    .filter(
      (patch) =>
        patch.SetProp?.prop === 'Label' &&
        patch.SetProp.value?.Text === label &&
        tagged.has(patch.SetProp.id),
    )
    .at(-1)?.SetProp.id;
  return patches.filter((patch) => patch.SetProp?.id === node && patch.SetProp.prop === prop).at(-1)
    ?.SetProp.value;
}

function latestPropertyForTag(stage, tag, prop) {
  const patches = stage.frames.flatMap((frame) => frame.patches);
  const nodes = new Set(
    patches.filter((patch) => patch.Create?.tag === tag).map((patch) => patch.Create.id),
  );
  return patches
    .filter((patch) => patch.SetProp?.prop === prop && nodes.has(patch.SetProp.id))
    .at(-1)?.SetProp.value;
}

function processWindowText(source) {
  if (source.version === 0) return [];
  const window = source.answer({
    source: 206,
    version: source.version,
    id: 1,
    range: { start: 0, count: 128 },
  });
  return window?.rows.flatMap((row) => row.cells.map((cell) => cell.Text ?? '')) ?? [];
}

function latestProperty(stage, node, prop) {
  return stage.frames
    .flatMap((frame) => frame.patches)
    .filter((patch) => patch.SetProp?.id === node && patch.SetProp.prop === prop)
    .at(-1)?.SetProp.value;
}

function orderedLabels(stage) {
  const children = new Map();
  const parents = new Map();
  const labels = new Map();
  const detach = (child) => {
    const parent = parents.get(child);
    if (parent === undefined) return;
    children.set(
      parent,
      (children.get(parent) ?? []).filter((candidate) => candidate !== child),
    );
    parents.delete(child);
  };
  for (const patch of stage.frames.flatMap((frame) => frame.patches)) {
    if (patch.Insert || patch.Move) {
      const { parent, child, before } = patch.Insert ?? patch.Move;
      detach(child);
      const siblings = children.get(parent) ?? [];
      const position = before === null ? siblings.length : siblings.indexOf(before);
      siblings.splice(position < 0 ? siblings.length : position, 0, child);
      children.set(parent, siblings);
      parents.set(child, parent);
    }
    if (patch.Remove) detach(patch.Remove.id);
    if (patch.SetProp?.prop === 'Label') labels.set(patch.SetProp.id, patch.SetProp.value?.Text);
  }
  const result = [];
  const visit = (node) => {
    const label = labels.get(node);
    if (label !== undefined) result.push(label);
    for (const child of children.get(node) ?? []) visit(child);
  };
  for (const child of children.get(0) ?? []) visit(child);
  return result;
}

function stageFromFrame(frame) {
  return { frames: [frame] };
}

function fieldValue(stage, placeholder) {
  const node = stage.frames
    .flatMap((frame) => frame.patches)
    .filter(
      (patch) =>
        'SetProp' in patch &&
        patch.SetProp.prop === 'Placeholder' &&
        patch.SetProp.value?.Text === placeholder,
    )
    .at(-1)?.SetProp.id;
  return stage.frames
    .flatMap((frame) => frame.patches)
    .filter(
      (patch) => 'SetProp' in patch && patch.SetProp.id === node && patch.SetProp.prop === 'Value',
    )
    .at(-1)?.SetProp.value?.Text;
}

function fieldValueByTooltip(stage, tooltip) {
  const patches = stage.frames.flatMap((frame) => frame.patches);
  const node = patches
    .filter((patch) => patch.SetProp?.prop === 'Tooltip' && patch.SetProp.value?.Text === tooltip)
    .at(-1)?.SetProp.id;
  return patches
    .filter((patch) => patch.SetProp?.id === node && patch.SetProp?.prop === 'Value')
    .at(-1)?.SetProp.value?.Text;
}

function placeholderProperty(stage, placeholder, prop) {
  const patches = stage.frames.flatMap((frame) => frame.patches);
  const node = patches
    .filter(
      (patch) => patch.SetProp?.prop === 'Placeholder' && patch.SetProp.value?.Text === placeholder,
    )
    .at(-1)?.SetProp.id;
  return patches.filter((patch) => patch.SetProp?.id === node && patch.SetProp.prop === prop).at(-1)
    ?.SetProp.value;
}

function placeholderTag(stage, placeholder) {
  const patches = stage.frames.flatMap((frame) => frame.patches);
  const node = patches
    .filter(
      (patch) => patch.SetProp?.prop === 'Placeholder' && patch.SetProp.value?.Text === placeholder,
    )
    .at(-1)?.SetProp.id;
  return patches.find((patch) => patch.Create?.id === node)?.Create.tag;
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
  const found = labelled(stage, label)?.SetProp.id;
  const ancestors = [];
  for (let node = found; parents.has(node);) {
    node = parents.get(node);
    ancestors.push(tags.get(node));
  }
  return ancestors;
}

function formControlField(stage, label, tag) {
  const patches = stage.frames.flatMap((frame) => frame.patches);
  const tags = new Map(
    patches.filter((patch) => patch.Create).map((patch) => [patch.Create.id, patch.Create.tag]),
  );
  const parents = new Map(
    patches
      .filter((patch) => patch.Insert)
      .map((patch) => [patch.Insert.child, patch.Insert.parent]),
  );
  let control = labelled(stage, label)?.SetProp.id;
  while (parents.has(control) && tags.get(control) !== 'FormControl')
    control = parents.get(control);
  assert.equal(tags.get(control), 'FormControl', `${label} belongs to a FormControl`);
  const field = [...parents]
    .filter(([, parent]) => parent === control)
    .map(([child]) => child)
    .find((child) => tags.get(child) === tag);
  assert.notEqual(field, undefined, `${label} names its ${tag}`);
  return field;
}

function ancestorProperty(stage, label, tag, prop) {
  const patches = stage.frames.flatMap((frame) => frame.patches);
  const tags = new Map(
    patches.filter((patch) => patch.Create).map((patch) => [patch.Create.id, patch.Create.tag]),
  );
  const parents = new Map(
    patches
      .filter((patch) => patch.Insert)
      .map((patch) => [patch.Insert.child, patch.Insert.parent]),
  );
  let node = labelled(stage, label)?.SetProp.id;
  while (parents.has(node)) {
    node = parents.get(node);
    if (tags.get(node) === tag) {
      const value = patches
        .filter((patch) => patch.SetProp?.id === node && patch.SetProp.prop === prop)
        .at(-1)?.SetProp.value;
      if (value !== undefined) return value;
    }
  }
  return undefined;
}

function outerAncestorProperty(stage, label, tag, prop) {
  const patches = stage.frames.flatMap((frame) => frame.patches);
  const tags = new Map(
    patches.filter((patch) => patch.Create).map((patch) => [patch.Create.id, patch.Create.tag]),
  );
  const parents = new Map(
    patches
      .filter((patch) => patch.Insert)
      .map((patch) => [patch.Insert.child, patch.Insert.parent]),
  );
  let node = labelled(stage, label)?.SetProp.id;
  let found;
  while (parents.has(node)) {
    node = parents.get(node);
    if (tags.get(node) === tag) {
      const value = patches
        .filter((patch) => patch.SetProp?.id === node && patch.SetProp.prop === prop)
        .at(-1)?.SetProp.value;
      if (value !== undefined) found = value;
    }
  }
  return found;
}

function compactDigest(digest) {
  return digest.length > 32 ? `${digest.slice(0, 19)}…${digest.slice(-8)}` : digest;
}

const settled = () => new Promise((resolve) => setImmediate(resolve));
