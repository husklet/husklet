import assert from 'node:assert/strict';
import fs from 'node:fs';
import test from 'node:test';
import {
  PROTOCOL_CAPABILITIES,
  PROTOCOL_REPLIES,
  PROTOCOL_REQUEST_CAPABILITIES,
  PROTOCOL_TOPICS,
  encodeRequest,
  validateFailure,
  validateReply,
  validateReplyFor,
  validateRequest,
  validateSnapshot,
} from '../dist/index.js';

test('generated validators follow authoritative request/reply/failure/snapshot roots', () => {
  assert.deepEqual(encodeRequest('workspace_info'), { call: 'workspace_info' });
  assert.deepEqual(
    validateRequest({
      call: 'terminal_write_pane',
      with: {
        slot: 'p1',
        generation: 2,
        revision: 3,
        contents: [0, 255],
      },
    }),
    {
      call: 'terminal_write_pane',
      with: {
        slot: 'p1',
        generation: 2,
        revision: 3,
        contents: [0, 255],
      },
    },
  );
  assert.deepEqual(validateReply({ reply: 'done' }), { reply: 'done' });
  assert.deepEqual(
    validateReplyFor('workspace_info', {
      reply: 'workspace',
      with: { name: 'dev', architecture: 'arm64', image: 'alpine' },
    }),
    { reply: 'workspace', with: { name: 'dev', architecture: 'arm64', image: 'alpine' } },
  );
  assert.throws(
    () => validateReplyFor('workspace_info', { reply: 'workspaces', with: [] }),
    /must be workspace/,
  );
  assert.deepEqual(
    validateFailure({ error: 'denied', capability: 'terminals:input', detail: 'not granted' }),
    { error: 'denied', capability: 'terminals:input', detail: 'not granted' },
  );
  assert.deepEqual(validateSnapshot({ snapshot: 'containers', of: [] }), {
    snapshot: 'containers',
    of: [],
  });
  assert.throws(
    () => validateRequest({ call: 'workspace_info', with: { invented: true } }),
    /absent/,
  );
  assert.throws(
    () => validateReply({ reply: 'container', with: { id: 'partial' } }),
    /name must be present/,
  );
  assert.throws(() => validateSnapshot({ snapshot: 'containers', of: [{}] }), /id must be present/);
  assert(PROTOCOL_CAPABILITIES.length >= 20);
  assert.equal(PROTOCOL_REPLIES.event_subscribe, 'done');
  assert.equal(PROTOCOL_REQUEST_CAPABILITIES.container_attach_terminal, 'containers:attach');
  assert.equal(PROTOCOL_REQUEST_CAPABILITIES.event_subscribe, null);
  assert.equal(
    PROTOCOL_TOPICS.find(({ wire }) => wire === 'pane-changes').snapshot,
    'pane_changes',
  );
});

test('generated RelativePath validation matches the Rust authority boundary', () => {
  const request = (path) => validateRequest({ call: 'filesystem_read', with: { path } });
  assert.deepEqual(request('./src//index.ts'), {
    call: 'filesystem_read',
    with: { path: './src//index.ts' },
  });
  for (const path of [
    '',
    '/etc/passwd',
    '\\\\host\\share',
    'C:\\Windows',
    '../secret',
    'a\\..\\secret',
    'a\0b',
  ])
    assert.throws(() => request(path), TypeError, path);
  assert.throws(() => request('é'.repeat(2049)), /4096 UTF-8 bytes/);
  assert.doesNotThrow(() => request('é'.repeat(2048)));
});

test('generated declarations correlate every authoritative request with its exact reply', () => {
  const declarations = fs.readFileSync(
    new URL('../src/generated-protocol.d.ts', import.meta.url),
    'utf8',
  );
  for (const [call, reply] of Object.entries(PROTOCOL_REPLIES)) {
    assert.match(
      declarations,
      new RegExp(`"${call}": Extract<WireReply, \\{ reply: "${reply}" \\}>;`),
    );
  }
  assert.match(declarations, /WireRequestParameters<C extends WireCall>/);
  assert.match(declarations, /WireReplyFor<C extends WireCall> = WireReplyByCall\[C\]/);
  assert.match(
    declarations,
    /WorkspaceEnvironmentSelector = \{ "workspace": string; "name": string \} \| \{ "all": boolean \}/,
  );
});

test('generated failure validation preserves unavailable as a typed wire category', () => {
  assert.deepEqual(validateFailure({ error: 'unavailable', detail: 'socket refused' }), {
    error: 'unavailable',
    detail: 'socket refused',
  });
  assert.throws(() => validateFailure({ error: 'unavailable' }), /detail must be present/);
});

test('integer widths and the cross-language lossless boundary are enforced before framing', () => {
  const safe = Number.MAX_SAFE_INTEGER;
  assert.deepEqual(
    validateRequest({
      call: 'extension_acquisition_cancel',
      with: { job: 'job-1', revision: safe },
    }),
    { call: 'extension_acquisition_cancel', with: { job: 'job-1', revision: safe } },
  );
  assert.throws(
    () =>
      validateRequest({
        call: 'extension_acquisition_cancel',
        with: { job: 'job-1', revision: safe + 1 },
      }),
    /integer from 0 through 9007199254740991/,
  );
  assert.throws(
    () =>
      validateRequest({
        call: 'terminal_write_pane',
        with: {
          slot: 'p1',
          generation: 1,
          revision: 1,
          contents: [256],
        },
      }),
    /integer from 0 through 255/,
  );
  assert.throws(
    () =>
      validateSnapshot({
        snapshot: 'pane_changes',
        of: {
          slot: 'p1',
          kind: 'terminal',
          generation: safe + 1,
          revision: 1,
          coalesced: 0,
        },
      }),
    /integer from 0 through 9007199254740991/,
  );
});

test('container consent selectors are exact and ambiguous shapes fail closed', () => {
  const base = {
    call: 'extension_install',
    with: {
      job: 'job-1',
      revision: 1,
      image_digest: `sha256:${'a'.repeat(64)}`,
      granted: ['containers:read'],
      containers: { selectors: [{ name: 'database' }], create: false },
      images: { read: [], use: [], pull: [], remove: [], prune_all_unused: false },
      networks: { selectors: [{ name: 'database' }], create: false },
      volumes: { selectors: [{ name: 'data' }], create: false },
      filesystem: {
        read: [],
        write: [{ exact: 'settings.json' }],
        create: [],
        delete: [],
        rename: [],
      },
      workspace_environment: {
        read: [{ workspace: 'dev', name: 'PGPASSWORD' }],
        write: [],
      },
    },
  };
  assert.deepEqual(validateRequest(base), base);
  assert.deepEqual(validateRequest(base).with.filesystem.write[0], { exact: 'settings.json' });
  const subtree = structuredClone(base);
  subtree.with.filesystem.read = [{ subtree: 'src' }];
  assert.deepEqual(validateRequest(subtree).with.filesystem.read, [{ subtree: 'src' }]);
  for (const selector of ['settings.json', { exact: 'settings.json', subtree: 'src' }]) {
    const invalid = structuredClone(base);
    invalid.with.filesystem.write = [selector];
    assert.throws(() => validateRequest(invalid), /filesystem|relative|selector|exact|subtree/i);
  }
  for (const selector of [
    { id: 'a'.repeat(32), name: 'database' },
    { name: 'database', invented: true },
    { all: true, name: 'database' },
  ]) {
    assert.throws(
      () =>
        validateRequest({
          ...base,
          with: { ...base.with, containers: { selectors: [selector], create: false } },
        }),
      /exactly one untagged variant/,
    );
  }
  for (const selector of [
    { id: 'a'.repeat(32), name: 'database' },
    { name: 'database', invented: true },
    { all: true, name: 'database' },
  ]) {
    assert.throws(
      () =>
        validateRequest({
          ...base,
          with: { ...base.with, networks: { selectors: [selector], create: false } },
        }),
      /exactly one untagged variant/,
    );
  }
  for (const selector of [
    { name: 'data', invented: true },
    { all: true, name: 'data' },
  ]) {
    assert.throws(
      () =>
        validateRequest({
          ...base,
          with: { ...base.with, volumes: { selectors: [selector], create: false } },
        }),
      /untagged variant|all/i,
    );
  }
});
