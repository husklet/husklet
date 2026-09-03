import { McpServer } from '@modelcontextprotocol/sdk/server/mcp.js';
import { Buffer } from 'node:buffer';
import { z } from 'zod';
import { workspace } from '@husklet/react';
import { FILE_BYTES_LIMIT, detailResult, fileRangeResult, fileResult, inventoryResult, logResult, publicError, result } from './bounds.js';
import { observePaneMutation, paneTools } from './panes.js';
export { paneXml, semanticXml } from './panes.js';

const id = z.string().min(1).max(256);
const containerIdentity = z.string().regex(/^(?:[0-9a-f]{32}|[0-9a-f]{64})$/, 'complete immutable container ID is required');
const executionIdentity = z.string().regex(/^[0-9a-f]{32}$/, 'complete immutable execution ID is required');
const imageDigest = z.string().regex(/^sha256:[0-9a-f]{64}$/, 'complete immutable image sha256 digest is required');
const networkIdentity = z.string().regex(/^[0-9a-f]{32}$/, 'complete immutable network ID is required');
const endpointAlias = z.string().min(1).max(253).regex(
  /^[A-Za-z0-9][A-Za-z0-9_.-]*$/,
  'endpoint alias must start with an ASCII alphanumeric and contain only ASCII alphanumerics, underscores, periods, or hyphens',
);
const endpointAliases = z.array(endpointAlias).max(64).default([]).superRefine((aliases, context) => {
  if (new Set(aliases).size !== aliases.length) {
    context.addIssue({ code: z.ZodIssueCode.custom, message: 'endpoint aliases must be unique' });
  }
});
const volumeGeneration = z.string().regex(/^[0-9a-f]{32}$/, 'complete immutable volume generation is required');
const extensionName = z.string().min(1).max(128).regex(/^[A-Za-z0-9][A-Za-z0-9_.-]*$/);
const extensionStatus = z.string().min(1).max(64).regex(/^(?:vacancy|standby|duty|fault:[0-9]+)$/);
const extensionInventoryCursor = z.object({
  name: extensionName,
  image_digest: imageDigest,
  status: extensionStatus,
}).strict();
const providerMountCursor = z.object({
  slot: z.string().min(1).max(256),
  generation: z.number().int().nonnegative().safe(),
  revision: z.number().int().nonnegative().safe(),
}).strict();
const extensionJob = z.string().min(1).max(128);
const extensionCapability = z.enum(['workspace-read', 'workspace-control', 'workspace-events', 'container-read', 'container-control', 'container-attach', 'image-read', 'image-write', 'volume-read', 'volume-write', 'network-read', 'network-write', 'terminal-read', 'terminal-control', 'terminal-output', 'pane-observe', 'pane-semantic-read', 'pane-semantic-control', 'extension-read', 'extension-control', 'extension-install', 'filesystem-read', 'filesystem-write', 'interface']);
const extensionGrant = z.array(extensionCapability).max(24).superRefine((granted, context) => {
  if (new Set(granted).size !== granted.length) context.addIssue({ code: z.ZodIssueCode.custom, message: 'granted capabilities must be unique' });
});
const acquisitionRevision = z.number().int().nonnegative().safe();
const signalName = z.string().min(1).max(32).refine(
  (value) => new TextEncoder().encode(value).byteLength <= 32,
  'signal exceeds 32 UTF-8 bytes',
);
const path = z.string().min(1).max(4096).refine(
  (value) => new TextEncoder().encode(value).byteLength <= 4096,
  'path exceeds 4096 UTF-8 bytes',
);
const fileContents = z.string().max(64 * 1024).refine(
  (value) => new TextEncoder().encode(value).byteLength <= 64 * 1024,
  'file contents exceed 65536 UTF-8 bytes',
);
const fileIdentity = z.string().min(1).max(256).regex(/^v1:[0-9a-f]+(?::[0-9a-f]+){6}$/);
const containerName = z.string().min(1).max(128).regex(/^[A-Za-z0-9][A-Za-z0-9_.-]*$/);
const paneTitle = z.string().refine((title) => title.trim().length > 0
  && Buffer.byteLength(title, 'utf8') <= 256 && !/[\u0000-\u001f\u007f-\u009f]/u.test(title),
'pane title must be nonblank and contain at most 256 UTF-8 bytes without control characters');
const hostname = z.string().min(1).max(253).regex(/^[A-Za-z0-9][A-Za-z0-9_.-]*$/);
const resourceName = z.string().min(1).max(255).regex(
  /^[A-Za-z0-9][A-Za-z0-9_.-]*$/,
  'resource name must start with an ASCII alphanumeric and contain only ASCII alphanumerics, underscores, periods, or hyphens',
);
const imageReference = z.string().min(1).max(512).superRefine((value, context) => {
  if (value.trim() !== value || /\s/.test(value)) {
    context.addIssue({ code: z.ZodIssueCode.custom, message: 'image reference must not contain whitespace' });
  }
  if (new TextEncoder().encode(value).byteLength > 512) {
    context.addIssue({ code: z.ZodIssueCode.custom, message: 'image reference exceeds 512 UTF-8 bytes' });
  }
});
const imagePullJob = z.string().min(1).max(20).regex(/^[1-9][0-9]*$/, 'image pull job must be a positive decimal identity');
const utf8Bytes = (value) => new TextEncoder().encode(value).byteLength;
const containerUser = z.string().min(1).max(256).refine(
  (value) => new TextEncoder().encode(value).byteLength <= 256,
  'container user exceeds 256 UTF-8 bytes',
);
const environmentValue = z.string().max(8192).refine(
  (value) => new TextEncoder().encode(value).byteLength <= 8192,
  'environment value exceeds 8192 UTF-8 bytes',
);
const environmentName = z.string().min(1).max(256).refine(
  (value) => !value.includes('=') && !value.includes('\0') && utf8Bytes(value) <= 256,
  'environment name must exclude equals and NUL and be at most 256 UTF-8 bytes',
);
const containerLabelValue = z.string().max(4096).refine(
  (value) => new TextEncoder().encode(value).byteLength <= 4096,
  'container label value exceeds 4096 UTF-8 bytes',
);
const containerLabelName = z.string().min(1).max(256).refine(
  (value) => new TextEncoder().encode(value).byteLength <= 256,
  'container label name exceeds 256 UTF-8 bytes',
);
const containerAbsolutePath = z.string().min(1).max(4096).startsWith('/').refine(
  (value) => utf8Bytes(value) <= 4096,
  'container path exceeds 4096 UTF-8 bytes',
);
const command = z.array(z.string().max(4096)).min(1).max(64).superRefine((argv, context) => {
  if (argv.length > 0 && argv[0].length === 0) context.addIssue({ code: z.ZodIssueCode.custom, message: 'the executable must not be empty' });
  if (argv.some((argument) => argument.includes('\0'))) context.addIssue({ code: z.ZodIssueCode.custom, message: 'command arguments cannot contain NUL' });
  if (argv.some((argument) => utf8Bytes(argument) > 4096)) context.addIssue({ code: z.ZodIssueCode.custom, message: 'each command argument must be at most 4096 UTF-8 bytes' });
  const bytes = argv.reduce((total, argument) => total + utf8Bytes(argument), 0);
  if (bytes > 32 * 1024) context.addIssue({ code: z.ZodIssueCode.custom, message: 'command exceeds 32768 bytes' });
});
const executionCursor = z.object({
  container_id: containerIdentity,
  running: z.boolean(),
  exit_code: z.number().int().safe(),
  pid: z.number().int().safe(),
  command,
  user: containerUser,
}).strict();
const optionalCommand = z.array(z.string().max(4096)).max(64);
const containerCreate = z.object({
  image: imageReference,
  name: containerName,
  hostname: hostname.nullable().default(null),
  entrypoint: command.nullable().default(null),
  command: optionalCommand.default([]),
  environment: z.array(z.tuple([environmentName, environmentValue])).max(256).default([]),
  working_directory: containerAbsolutePath.nullable().default(null),
  user: containerUser.nullable().default(null),
  labels: z.array(z.tuple([containerLabelName, containerLabelValue])).max(128).default([]),
  mounts: z.array(z.object({ volume: resourceName, target: containerAbsolutePath, read_only: z.boolean().default(false) }).strict()).max(64).default([]),
  network: resourceName.nullable().default(null),
  ports: z.array(z.object({ container: z.number().int().min(1).max(65535), host: z.number().int().min(1).max(65535).nullable().default(null), protocol: z.enum(['tcp', 'udp']) }).strict()).max(64).default([]),
  memory_mb: z.number().int().min(1).max(1_048_576).nullable().default(null),
  cpus: z.number().int().min(1).max(256).nullable().default(null),
  pids_limit: z.number().int().min(1).max(1_000_000).nullable().default(null),
}).strict().superRefine((spec, context) => {
  const issue = (message) => context.addIssue({ code: z.ZodIssueCode.custom, message });
  const argv = [...(spec.entrypoint ?? []), ...spec.command];
  if (spec.command.length > 0 && spec.command[0].length === 0) issue('command executable must not be empty');
  if (argv.some((argument) => argument.includes('\0'))
    || argv.some((argument) => utf8Bytes(argument) > 4096)
    || argv.reduce((total, argument) => total + utf8Bytes(argument), 0) > 32 * 1024) issue('entrypoint and command must be NUL-free, at most 4096 bytes per argument, and at most 32768 bytes in aggregate');
  const normalized = (value) => !value.split('/').some((part) => part === '.' || part === '..');
  if (spec.working_directory != null && !normalized(spec.working_directory)) issue('working_directory must be normalized');
  if (spec.mounts.some(({ target }) => !normalized(target))) issue('mount targets must be normalized');
  if (spec.user?.includes('\0') || spec.environment.some(([, value]) => value.includes('\0'))
    || spec.labels.some(([name, value]) => name.includes('\0') || value.includes('\0'))) issue('text fields cannot contain NUL');
  const unique = (pairs) => new Set(pairs.map(([name]) => name)).size === pairs.length;
  if (!unique(spec.environment) || !unique(spec.labels)) issue('environment and label names must be unique');
  if (new Set(spec.ports.map(({ container, protocol }) => `${container}/${protocol}`)).size !== spec.ports.length) issue('container ports must be unique');
});
const nullable = (schema) => schema.nullable();
const absolutePath = z.string().min(1).max(4096).startsWith('/');
const workspaceConfiguration = z.object({
  name: z.string().min(1).max(128).refine((value) => value.trim() === value),
  image: imageReference,
  architecture: z.enum(['arm64', 'amd64']),
  storage: nullable(absolutePath),
  shell: nullable(z.string().max(4096)),
  cpus: nullable(z.number().int().min(1).max(1024)),
  memory_mb: nullable(z.number().int().min(1).max(1024 * 1024)),
  environment: z.array(z.tuple([z.string().min(1).max(256), z.string().max(8192)])).max(256),
  mounts: z.array(z.object({ host: absolutePath, container: absolutePath, read_only: z.boolean() }).strict()).max(128),
  docker_socket: z.boolean(),
  scrollback: nullable(z.number().int().min(0).max(10_000_000)),
  vpn: nullable(z.string().min(1).max(2048)),
  execution_lifetime: z.enum(['persisted', 'live', 'ephemeral']),
  terminal: z.object({
    font_family: nullable(z.string().min(1).max(256)), font_size: nullable(z.number().int().min(1).max(256)),
    foreground: nullable(z.string().max(64)), background: nullable(z.string().max(64)),
    cursor_shape: nullable(z.string().max(64)), cursor_blink: nullable(z.boolean()),
  }).strict(),
}).strict();
const workspaceGeneration = z.string().regex(/^[0-9a-f]{32}$/, 'complete immutable workspace generation is required');
const legacyWorkspaceConfiguration = workspaceConfiguration.extend({ generation: z.literal('') }).strict();
const workspaceUpdate = z.object({ name: id, generation: workspaceGeneration, configuration: workspaceConfiguration, confirm: z.literal(true) }).strict()
  .superRefine(({ name, configuration }, context) => {
    if (name !== configuration.name) context.addIssue({ code: z.ZodIssueCode.custom, message: 'renaming a workspace is not supported' });
  });
const empty = z.object({}).strict();
const slot = z.object({ slot: id }).strict();
const define = (name, description, inputSchema, run, pack = result) => ({ name, description, inputSchema, run: async (input) => {
  try { return pack(await run(input)); } catch (error) { throw publicError(error); }
} });
const inventory = (field) => (value) => inventoryResult(value, field);
const PANE_INPUT_BYTES = 64 * 1024;
const BASE64_INPUT_CHARS = Math.ceil(PANE_INPUT_BYTES / 3) * 4;
const canonicalBase64 = /^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$/;
const decodeTerminalBytes = (encoded) => {
  if (typeof encoded !== 'string' || encoded.length === 0 || encoded.length > BASE64_INPUT_CHARS
      || !canonicalBase64.test(encoded)) throw new TypeError('input must be canonical padded base64');
  const decoded = Buffer.from(encoded, 'base64');
  if (decoded.length > PANE_INPUT_BYTES) throw new RangeError(`decoded terminal input exceeds ${PANE_INPUT_BYTES} bytes`);
  if (decoded.toString('base64') !== encoded) throw new TypeError('input must be canonical padded base64');
  return Uint8Array.from(decoded);
};
const terminalText = z.string().max(PANE_INPUT_BYTES).refine(
  (value) => new TextEncoder().encode(value).byteLength <= PANE_INPUT_BYTES,
  `terminal input exceeds ${PANE_INPUT_BYTES} UTF-8 bytes`,
);
const terminalBytes = z.object({
  slot: id,
  input_base64: z.string().min(4).max(BASE64_INPUT_CHARS).superRefine((encoded, context) => {
    try { decodeTerminalBytes(encoded); } catch (error) {
      context.addIssue({ code: z.ZodIssueCode.custom, message: error.message });
    }
  }),
}).strict();

const workspaceMutation = z.discriminatedUnion('operation', [
  z.object({ operation: z.literal('create'), configuration: workspaceConfiguration, timeout_ms: z.number().int().min(1).max(30_000).default(30_000) }).strict(),
  z.object({ operation: z.literal('start'), name: id, timeout_ms: z.number().int().min(1).max(30_000).default(30_000) }).strict(),
  z.object({ operation: z.literal('stop'), name: id, timeout_ms: z.number().int().min(1).max(30_000).default(30_000) }).strict(),
  z.object({ operation: z.literal('delete'), name: id, generation: workspaceGeneration, confirm: z.literal(true), timeout_ms: z.number().int().min(1).max(30_000).default(30_000) }).strict(),
]);

async function observeWorkspaceMutation(api, input) {
  const action = input.operation === 'delete' ? 'remove' : input.operation;
  const workspaceName = input.operation === 'create' ? input.configuration.name : input.name;
  let settle;
  let fail;
  const observed = new Promise((resolve, reject) => { settle = resolve; fail = reject; });
  let timer;
  const dispose = await api.watchWorkspaceLifecycle((change) => {
    if (change?.workspace === workspaceName && change?.action === action) settle(change);
  });
  try {
    timer = setTimeout(() => fail(new Error(`timed out waiting for ${action} lifecycle change for workspace ${workspaceName}`)), input.timeout_ms);
    let result;
    if (input.operation === 'create') result = await api.create(input.configuration);
    else if (input.operation === 'delete') { await api.delete(input.name, input.generation); result = { done: true }; }
    else { await api[input.operation](input.name); result = { done: true }; }
    return { result, change: await observed };
  } finally {
    clearTimeout(timer);
    await dispose();
  }
}

async function commitExtension(api, operation, job, revision, granted) {
  const status = await api.extensions.acquisition(job);
  if (status?.job !== job || status?.revision !== revision || status?.state !== 'ready' || status?.candidate == null) {
    throw new Error(`extension ${operation} requires the exact ready acquisition job and revision`);
  }
  const candidate = status.candidate;
  if (!imageDigest.safeParse(candidate.image_digest).success) throw new Error('ready extension candidate has no complete immutable image digest');
  const requested = new Set(candidate.requested ?? []);
  const widened = granted.find((capability) => !requested.has(capability));
  if (widened != null) throw new Error(`grant ${widened} was not requested by the observed extension candidate`);
  const installed = await api.extensions[operation](job, revision, granted);
  if (installed?.name !== candidate.name || installed?.image_digest !== candidate.image_digest) {
    throw new Error(`host returned an extension identity that does not match the observed candidate after ${operation}`);
  }
  return { ...installed, job, revision, consented_grants: granted };
}

export function tools(api) {
  const definitions = [
    define('husklet_workspace_info', 'Describe the hosting workspace.', empty, () => api.info()),
    define('husklet_workspace_list', 'List workspace summaries, failing closed if MCP cannot return the complete host inventory.', empty, () => api.list(), inventory()),
    define('husklet_workspace_inspect', 'Inspect one complete named workspace configuration; secret-named environment values are redacted and local clipping fails closed.', z.object({ name: id }).strict(), ({ name }) => api.inspect(name), detailResult),
    define('husklet_workspace_create', 'Create one workspace from a complete bounded configuration.', z.object({ configuration: workspaceConfiguration }).strict(), ({ configuration }) => api.create(configuration)),
    define('husklet_workspace_adopt', 'Assign immutable identity to the exact unchanged legacy workspace after explicit confirmation.', z.object({ configuration: legacyWorkspaceConfiguration, confirm: z.literal(true) }).strict(), ({ configuration }) => api.adopt(configuration)),
    define('husklet_workspace_update', 'Replace the exact observed stopped workspace generation after explicit confirmation.', workspaceUpdate, ({ name, generation, configuration }) => api.update(name, generation, configuration)),
    ...['start', 'stop', 'restart'].map((action) => define(`husklet_workspace_${action}`, `${action} a named workspace.`, z.object({ name: id }).strict(), async ({ name }) => { await api[action](name); return { done: true }; })),
    define('husklet_workspace_delete', 'Delete the exact observed stopped workspace generation after explicit confirmation.', z.object({ name: id, generation: workspaceGeneration, confirm: z.literal(true) }).strict(), async ({ name, generation }) => { await api.delete(name, generation); return { done: true, name, generation }; }),
    define('husklet_workspace_mutate_wait', 'Arm lifecycle observation, perform one bounded workspace mutation, then return its result and matching authoritative change.', workspaceMutation, (input) => observeWorkspaceMutation(api, input)),
    define('husklet_extension_list', 'List installed extension records and lifecycle status, failing closed if MCP cannot return the complete host inventory.', empty, () => api.extensions.list(), inventory()),
    define('husklet_extension_inspect', 'Inspect one complete installed extension record without local clipping.', z.object({ name: extensionName }).strict(), ({ name }) => api.extensions.inspect(name), detailResult),
    define('husklet_extension_enable', 'Enable the exact inspected extension image after explicit confirmation.', z.object({ name: extensionName, image_digest: imageDigest, confirm: z.literal(true) }).strict(), async ({ name, image_digest }) => { await api.extensions.enable(name, image_digest); return { done: true, name, image_digest }; }),
    define('husklet_extension_disable', 'Disable the exact inspected extension image after explicit confirmation.', z.object({ name: extensionName, image_digest: imageDigest, confirm: z.literal(true) }).strict(), async ({ name, image_digest }) => { await api.extensions.disable(name, image_digest); return { done: true, name, image_digest }; }),
    define('husklet_extension_remove', 'Forget the exact inspected extension image after explicit confirmation; this does not install or pull images.', z.object({ name: extensionName, image_digest: imageDigest, confirm: z.literal(true) }).strict(), async ({ name, image_digest }) => { await api.extensions.remove(name, image_digest); return { done: true, name, image_digest }; }),
    define('husklet_extension_acquire', 'Start bounded asynchronous inspection of one image reference after explicit confirmation.', z.object({ reference: imageReference, confirm: z.literal(true) }).strict(), ({ reference }) => api.extensions.startAcquisition(reference)),
    define('husklet_extension_acquisition', 'Read complete bounded acquisition progress, candidate digest, installed digest observed for consent, manifest identity, and requested grants without local clipping.', z.object({ job: extensionJob }).strict(), ({ job }) => api.extensions.acquisition(job), detailResult),
    define('husklet_extension_acquisition_cancel', 'Cancel one observed acquisition revision after explicit confirmation.', z.object({ job: extensionJob, revision: acquisitionRevision, confirm: z.literal(true) }).strict(), async ({ job, revision }) => { await api.extensions.cancelAcquisition(job, revision); return { done: true, job, revision }; }),
    define('husklet_extension_install', 'Re-read and consent to the exact ready job/revision/digest, refuse grants outside its manifest request, atomically install, and verify the returned immutable identity.', z.object({ job: extensionJob, revision: acquisitionRevision, granted: extensionGrant, confirm: z.literal(true) }).strict(), ({ job, revision, granted }) => commitExtension(api, 'install', job, revision, granted), detailResult),
    define('husklet_extension_update', 'Re-read and consent to the exact ready job/revision/digest, refuse grants outside its manifest request, atomically update, and verify the returned immutable identity.', z.object({ job: extensionJob, revision: acquisitionRevision, granted: extensionGrant, confirm: z.literal(true) }).strict(), ({ job, revision, granted }) => commitExtension(api, 'update', job, revision, granted), detailResult),
    define('husklet_extension_provider_wait', 'Wait for an actually mounted extension/provider occupant, or its removal, using exact pane generation/revision fencing; this does not claim enablement alone registered or mounted a provider.', z.object({ extension: extensionName, provider: extensionName, state: z.enum(['mounted', 'unmounted']).default('mounted'), after: providerMountCursor.nullable().default(null), timeout_ms: z.number().int().min(1).max(30_000).default(30_000) }).strict(), ({ extension, provider, state, after, timeout_ms }) => api.extensions.waitForProviderMount(extension, provider, { state, after, timeoutMs: timeout_ms }), detailResult),
    define('husklet_container_list', 'List containers, failing closed if MCP cannot return the complete host inventory.', empty, () => api.containers.list(), inventory()),
    define('husklet_container_inspect', 'Inspect one complete container record without local clipping.', z.object({ id }).strict(), ({ id: value }) => api.containers.inspect(value), detailResult),
    define('husklet_container_processes', 'Read a bounded timestamped process snapshot bound to the complete immutable container ID actually sampled; scope says initial or full namespace, and PIDs are snapshot-local and reusable.', z.object({ id }).strict(), ({ id: value }) => api.containers.processes(value)),
    define('husklet_container_execution', 'Inspect one complete bounded execution by its complete immutable ID without local clipping.', z.object({ id: executionIdentity }).strict(), ({ id: value }) => api.containers.execution(value), detailResult),
    define('husklet_execution_list', 'List the bounded durable execution catalogue; MCP promotes its truncation marker if it omits rows.', empty, () => api.containers.executions(), inventory('executions')),
    define('husklet_execution_logs', 'Replay bounded captured stdout/stderr bytes for one immutable execution ID; eof means the execution was complete before replay, while per-stream flags report host or MCP truncation.', z.object({ id: executionIdentity, stdout: z.boolean().default(true), stderr: z.boolean().default(true) }).strict().refine(({ stdout, stderr }) => stdout || stderr, 'stdout or stderr is required'), ({ id: value, stdout, stderr }) => api.containers.executionLogs(value, { stdout, stderr }), logResult),
    define('husklet_execution_wait', 'Wait up to 30 seconds for one immutable execution ID to stop and return its final state.', z.object({ id: executionIdentity, timeout_ms: z.number().int().min(1).max(30_000).default(30_000) }).strict(), ({ id: value, timeout_ms }) => api.containers.waitExecution(value, { timeoutMs: timeout_ms })),
    define('husklet_execution_signal', 'Signal one immutable execution ID after explicit confirmation without signaling its owning container; snapshot PIDs are never accepted.', z.object({ id: executionIdentity, signal: signalName, confirm: z.literal(true) }).strict(), async ({ id: value, signal }) => { await api.containers.signalExecution(value, signal); return { done: true, id: value, signal }; }),
    define('husklet_execution_remove', 'Remove one stopped execution record selected by its complete immutable ID, and its captured output, after explicit confirmation.', z.object({ id: executionIdentity, confirm: z.literal(true) }).strict(), async ({ id: value }) => { await api.containers.removeExecution(value); return { done: true, id: value }; }),
    define('husklet_container_logs', 'Read bounded captured stdout/stderr bytes from one complete immutable container ID; this is log replay, not the interpreted terminal screen/history.', z.object({ id: containerIdentity, stdout: z.boolean().default(true), stderr: z.boolean().default(true) }).strict().refine(({ stdout, stderr }) => stdout || stderr, 'stdout or stderr is required'), ({ id: value, stdout, stderr }) => api.containers.logs(value, { stdout, stderr }), logResult),
    define('husklet_container_create', 'Create a bounded configured container from a local image; mounts are named volumes and published ports bind loopback only.', containerCreate, async (spec) => ({ id: await api.containers.create(spec) })),
    define('husklet_container_exec', 'Execute a bounded argv vector in one complete immutable running container ID without shell parsing.', z.object({ id: containerIdentity, command, user: containerUser.optional(), working_directory: containerAbsolutePath.optional() }).strict(), async ({ id: value, command: argv, user, working_directory: workingDirectory }) => ({ id: await api.containers.exec(value, { command: argv, user, workingDirectory }) })),
    define('husklet_container_attach_terminal', 'Open an ephemeral GUI terminal running an exact bounded argv in a complete immutable container ID; the process is killed when the pane disconnects.', z.object({ id: containerIdentity, command }).strict(), ({ id: value, command: argv }) => api.containers.attachTerminal(value, argv)),
    ...['start', 'pause', 'unpause', 'restart'].map((action) => define(
      `husklet_container_${action}`,
      `${action} one container selected by its complete immutable ID; names and prefixes are refused.`,
      z.object({ id: containerIdentity }).strict(),
      async ({ id: value }) => { await api.containers[action](value); return { done: true }; },
    )),
    define('husklet_container_rename', 'Atomically assign a unique name to one complete immutable container ID.', z.object({ id: containerIdentity, name: containerName }).strict(), async ({ id: value, name }) => { await api.containers.rename(value, name); return { done: true }; }),
    define('husklet_container_stop', 'Stop one complete immutable container ID after explicit confirmation; names and prefixes are refused.', z.object({ id: containerIdentity, confirm: z.literal(true) }).strict(), async ({ id: value }) => { await api.containers.stop(value); return { done: true, id: value }; }),
    define('husklet_container_remove', 'Remove one complete immutable container ID after explicit confirmation; names and prefixes are refused.', z.object({ id: containerIdentity, confirm: z.literal(true) }).strict(), async ({ id: value }) => { await api.containers.remove(value); return { done: true, id: value }; }),
    define('husklet_container_kill', 'Signal one complete immutable container ID after explicit confirmation; names, prefixes and process PIDs are refused.', z.object({ id: containerIdentity, signal: signalName, confirm: z.literal(true) }).strict(), async ({ id: value, signal }) => { await api.containers.kill(value, signal); return { done: true, id: value, signal }; }),
    define('husklet_volume_list', 'List local volume summaries, failing closed if MCP cannot return the complete host inventory.', empty, () => api.volumes.list(), inventory()),
    define('husklet_volume_inspect', 'Inspect one complete local volume record without local clipping.', z.object({ name: resourceName }).strict(), ({ name }) => api.volumes.inspect(name), detailResult),
    define('husklet_volume_create', 'Create one named local volume.', z.object({ name: resourceName }).strict(), ({ name }) => api.volumes.create(name)),
    define('husklet_volume_remove', 'Remove one exact observed volume generation after explicit confirmation.', z.object({ name: resourceName, generation: volumeGeneration, confirm: z.literal(true) }).strict(), async ({ name, generation }) => { await api.volumes.remove(name, generation); return { done: true, name, generation }; }),
    define('husklet_network_list', 'List local network summaries, failing closed if MCP cannot return the complete host inventory.', empty, () => api.networks.list(), inventory()),
    define('husklet_network_inspect', 'Inspect one complete local network record without local clipping.', z.object({ reference: id }).strict(), ({ reference }) => api.networks.inspect(reference), detailResult),
    define('husklet_network_create', 'Create one named local network.', z.object({ name: resourceName }).strict(), ({ name }) => api.networks.create(name)),
    define('husklet_network_remove', 'Remove one immutable network ID after explicit confirmation; names and prefixes are refused.', z.object({ reference: networkIdentity, confirm: z.literal(true) }).strict(), async ({ reference }) => { await api.networks.remove(reference); return { done: true, reference }; }),
    define('husklet_network_connect', 'Connect one immutable container ID to one immutable network ID with optional bounded DNS aliases.', z.object({ reference: networkIdentity, container: containerIdentity, aliases: endpointAliases }).strict(), async ({ reference, container, aliases }) => { await api.networks.connect(reference, container, { aliases }); return { done: true }; }),
    define('husklet_network_disconnect', 'Disconnect one immutable container ID from one immutable network ID after explicit confirmation.', z.object({ reference: networkIdentity, container: containerIdentity, confirm: z.literal(true) }).strict(), async ({ reference, container }) => { await api.networks.disconnect(reference, container); return { done: true, reference, container }; }),
    define('husklet_image_list', 'List local image summaries, failing closed if MCP cannot return the complete host inventory.', empty, () => api.images.list(), inventory()),
    define('husklet_image_inspect', 'Inspect one complete local image record without local clipping.', z.object({ reference: id }).strict(), ({ reference }) => api.images.inspect(reference), detailResult),
    define('husklet_image_pull', 'Pull one explicit image reference.', z.object({ reference: id }).strict(), ({ reference }) => api.images.pull(reference)),
    define('husklet_image_pull_start', 'Start a bounded asynchronous image pull. Prefer this observable workflow over the synchronous compatibility tool.', z.object({ reference: imageReference }).strict(), ({ reference }) => api.images.startPull(reference)),
    define('husklet_image_pull_status', 'Read the latest bounded status for one exact image-pull job.', z.object({ job: imagePullJob }).strict(), async ({ job }) => {
      const status = await api.images.pullStatus(job);
      if (status.job !== job) throw new Error(`host returned image pull job ${status.job}, expected ${job}`);
      return status;
    }, detailResult),
    define('husklet_image_pull_cancel', 'Cancel one active image-pull job; cancellation is safe and does not require destructive confirmation.', z.object({ job: imagePullJob }).strict(), async ({ job }) => { await api.images.cancelPull(job); return { done: true, job }; }),
    define('husklet_image_remove', 'Remove one immutable image digest after explicit confirmation; mutable tags and partial digests are refused.', z.object({ reference: imageDigest, confirm: z.literal(true) }).strict(), async ({ reference }) => { await api.images.remove(reference); return { done: true, reference }; }),
    define('husklet_image_prune', 'Prune unused images after explicit confirmation.', z.object({ confirm: z.literal(true) }).strict(), () => api.images.prune()),
    define('husklet_terminal_tabs', 'List terminal tabs, failing closed if MCP cannot return the complete host inventory.', empty, () => api.terminal.tabs(), inventory()),
    define('husklet_terminal_topology', 'Read complete terminal split topology, failing closed rather than omitting nested layout.', empty, () => api.terminal.topology(), inventory()),
    define('husklet_terminal_read', 'Read at most 500 lines of interpreted terminal screen/history with cursor and grid state; this is not raw stdout/stderr.', z.object({ slot: id, lines: z.number().int().min(1).max(500) }).strict(), ({ slot: value, lines }) => api.terminal.read(value, lines)),
    define('husklet_terminal_write', 'Write UTF-8 input to one exact observed terminal pane; this does not spawn a shell command.', z.object({ slot: id, generation: acquisitionRevision, revision: acquisitionRevision, input: terminalText }).strict(), async ({ slot: value, generation, revision, input }) => { await api.terminal.writeInput(value, generation, revision, input); return { done: true }; }),
    define('husklet_terminal_write_bytes', 'Write arbitrary bytes to one exact observed terminal pane from canonical padded base64, including control and non-UTF8 bytes.', terminalBytes.extend({ generation: acquisitionRevision, revision: acquisitionRevision }).strict(), async ({ slot: value, generation, revision, input_base64: encoded }) => { await api.terminal.writeInput(value, generation, revision, decodeTerminalBytes(encoded)); return { done: true }; }),
    define('husklet_terminal_open', 'Open a terminal tab, titled Terminal when omitted.', z.object({ title: z.string().max(256).optional() }).strict(), ({ title }) => api.terminal.openTab(title ?? 'Terminal')),
    define('husklet_terminal_split', 'Split only the exact observed pane generation and revision beside or below.', z.object({ slot: id, generation: acquisitionRevision, revision: acquisitionRevision, division: z.enum(['beside', 'below']) }).strict(), ({ slot: value, generation, revision, division }) => api.terminal.splitObserved(value, generation, revision, division)),
    define('husklet_terminal_spawn', "Run a bounded exact argv vector through one exact observed terminal generation and revision; generated quoting prevents arguments from becoming shell syntax.", z.object({ slot: id, generation: acquisitionRevision, revision: acquisitionRevision, command }).strict(), async ({ slot: value, generation, revision, command: argv }) => { await api.terminal.spawnObserved(value, generation, revision, argv); return { done: true }; }),
    define('husklet_terminal_focus', 'Focus one exact observed pane generation and revision.', z.object({ slot: id, generation: acquisitionRevision, revision: acquisitionRevision }).strict(), async ({ slot: value, generation, revision }) => { await api.terminal.focusObserved(value, generation, revision); return { done: true }; }),
    define('husklet_terminal_retitle', 'Retitle the tab containing one exact observed pane generation and revision.', z.object({ slot: id, generation: acquisitionRevision, revision: acquisitionRevision, title: paneTitle }).strict(), async ({ slot: value, generation, revision, title }) => { await api.terminal.retitleObserved(value, generation, revision, title); return { done: true }; }),
    define('husklet_terminal_resize', 'Resize only the exact observed terminal generation and revision.', z.object({ slot: id, generation: acquisitionRevision, revision: acquisitionRevision, columns: z.number().int().min(1).max(1000), rows: z.number().int().min(1).max(1000) }).strict(), async ({ slot: value, generation, revision, columns, rows }) => { await api.terminal.resizeGridObserved(value, generation, revision, columns, rows); return { done: true }; }),
    define('husklet_terminal_ratio', 'Set the split share only for the exact observed pane generation and revision.', z.object({ slot: id, generation: acquisitionRevision, revision: acquisitionRevision, ratio: z.number().min(0.05).max(0.95) }).strict(), async ({ slot: value, generation, revision, ratio }) => { await api.terminal.ratioObserved(value, generation, revision, ratio); return { done: true }; }),
    define('husklet_terminal_switch_occupant', 'Switch one exact observed pane generation and revision between its retained terminal and a live named extension provider.', z.object({ slot: id, generation: acquisitionRevision, revision: acquisitionRevision, target: z.union([z.object({ kind: z.literal('terminal') }).strict(), z.object({ kind: z.literal('surface'), extension: extensionName, provider: extensionName }).strict()]) }).strict(), async ({ slot: value, generation, revision, target }) => { await api.terminal.switchOccupantObserved(value, generation, revision, target); return { done: true }; }),
    define('husklet_terminal_close', 'Close only the exact observed pane generation and revision after explicit confirmation.', z.object({ slot: id, generation: acquisitionRevision, revision: acquisitionRevision, confirm: z.literal(true) }).strict(), async ({ slot: value, generation, revision }) => { await api.terminal.closeObserved(value, generation, revision); return { done: true, slot: value, generation, revision }; }),
    define('husklet_file_list', 'List a workspace-relative directory.', z.object({ path }).strict(), ({ path: value }) => api.files.list(value)),
    define('husklet_file_stat', 'Read bounded metadata for one workspace-relative path without reading contents.', z.object({ path }).strict(), ({ path: value }) => api.files.stat(value)),
    define('husklet_file_read', `Read one workspace-confined file only when its complete contents fit the ${FILE_BYTES_LIMIT}-byte MCP whole-read limit; larger successful host reads fail closed.`, z.object({ path }).strict(), ({ path: value }) => api.files.read(value), fileResult),
    define('husklet_file_read_range', `Read at most ${FILE_BYTES_LIMIT} bytes from one stable opened workspace file. Pass the first page identity as observed on every later page to reject concurrent replacement.`, z.object({ path, offset: z.number().int().nonnegative().max(1_040_384).default(0), limit: z.number().int().min(1).max(FILE_BYTES_LIMIT).default(FILE_BYTES_LIMIT), observed: fileIdentity.nullable().default(null) }).strict(), ({ path: value, offset, limit, observed }) => api.files.readRange(value, offset, limit, observed), fileRangeResult),
    define('husklet_file_write', 'Write at most 65536 UTF-8 bytes to a workspace-relative file.', z.object({ path, contents: fileContents }).strict(), async ({ path: value, contents }) => { await api.files.write(value, new TextEncoder().encode(contents)); return { done: true }; }),
    define('husklet_file_create_observed', 'Atomically create only if the workspace-relative path is still absent; never overwrite an existing entry.', z.object({ path, contents: fileContents }).strict(), async ({ path: value, contents }) => ({ identity: await api.files.createObserved(value, new TextEncoder().encode(contents)) })),
    define('husklet_file_mkdir', 'Create one workspace-relative directory.', z.object({ path }).strict(), async ({ path: value }) => { await api.files.mkdir(value); return { done: true }; }),
    define('husklet_file_rename', 'Rename one workspace-relative entry without overwriting.', z.object({ from: path, to: path }).strict(), async ({ from, to }) => { await api.files.rename(from, to); return { done: true }; }),
    define('husklet_file_remove', 'Remove one file or empty directory after explicit confirmation.', z.object({ path, confirm: z.literal(true) }).strict(), async ({ path: value }) => { await api.files.remove(value); return { done: true, path: value }; }),
  ];
  if (typeof api.terminal?.panes === 'function') definitions.push(define(
    'husklet_pane_list',
    'List every inspectable terminal, extension surface, and native pane without reading its contents.',
    empty,
    () => api.terminal.panes(),
    inventory('panes'),
  ));
  if (typeof api.watchPaneChanges === 'function') definitions.push(define(
    'husklet_pane_wait',
    'Wait for bounded pane-change metadata; fetch a snapshot after notification.',
    z.object({ slot: id.optional(), after_generation: acquisitionRevision.optional(), after_revision: acquisitionRevision.optional(), timeout_ms: z.number().int().min(1).max(30_000).default(30_000) }).strict()
      .superRefine(({ slot, after_generation, after_revision }, context) => {
        const cursor = after_generation != null || after_revision != null;
        if (cursor && (slot == null || after_generation == null || after_revision == null)) context.addIssue({ code: z.ZodIssueCode.custom, message: 'slot-specific pane waits require slot, after_generation, and after_revision together' });
      }),
    ({ slot: wanted, after_generation, after_revision, timeout_ms: timeout }) => new Promise((resolve, reject) => {
      let stop;
      let settled = false;
      const finish = (value, error) => {
        if (settled) return;
        settled = true;
        clearTimeout(timer);
        Promise.resolve(stop?.()).then(() => error ? reject(error) : resolve(value), reject);
      };
      const timer = setTimeout(() => finish({ changed: false }), timeout);
      api.watchPaneChanges((change) => {
        const newer = after_generation == null
          || change.generation > after_generation
          || (change.generation === after_generation && change.revision > after_revision);
        if ((wanted == null || change.slot === wanted) && newer) finish({ changed: true, change });
      }).then((dispose) => { stop = dispose; if (settled) void dispose(); }, (error) => finish(undefined, error));
    }),
  ));
  if (typeof api.watchPaneChanges === 'function') definitions.push(
    define('husklet_terminal_write_wait', 'Atomically arm pane observation, write UTF-8 input to one exact terminal cursor, and return the matching change.', z.object({ slot: id, generation: acquisitionRevision, revision: acquisitionRevision, input: terminalText, timeout_ms: z.number().int().min(1).max(30_000).default(30_000) }).strict(), ({ slot: value, generation, revision, input, timeout_ms: timeout }) => observePaneMutation(api.watchPaneChanges.bind(api), { slot: value, generation, revision, timeout }, async () => { await api.terminal.writeInput(value, generation, revision, input); return { done: true }; })),
    define('husklet_terminal_write_bytes_wait', 'Atomically arm pane observation, write canonical-base64 bytes to one exact terminal cursor, and return the matching change.', terminalBytes.extend({ generation: acquisitionRevision, revision: acquisitionRevision, timeout_ms: z.number().int().min(1).max(30_000).default(30_000) }).strict(), ({ slot: value, generation, revision, input_base64: encoded, timeout_ms: timeout }) => observePaneMutation(api.watchPaneChanges.bind(api), { slot: value, generation, revision, timeout }, async () => { await api.terminal.writeInput(value, generation, revision, decodeTerminalBytes(encoded)); return { done: true }; })),
  );
  if (typeof api.watchWorkspaceEvents === 'function') definitions.push(define(
    'husklet_workspace_event_wait',
    'Wait once for a bounded permission-gated window keyboard, focus, or pane-addressed pointer event batch.',
    z.object({
      kind: z.enum(['key', 'focus', 'pointer']).optional(),
      slot: z.string().min(1).max(128).optional(),
      phase: z.enum(['move', 'enter', 'leave', 'press', 'release', 'click', 'context', 'scroll']).optional(),
      timeout_ms: z.number().int().min(1).max(30_000).default(30_000),
    }).strict().superRefine(({ kind, slot, phase }, context) => {
      if (slot != null && kind == null) context.addIssue({ code: z.ZodIssueCode.custom, message: 'slot filtering requires an explicit event kind' });
      if (phase != null && kind !== 'pointer') context.addIssue({ code: z.ZodIssueCode.custom, message: 'phase filtering requires kind pointer' });
    }),
    ({ kind, slot, phase, timeout_ms: timeout }) => new Promise((resolve, reject) => {
      let stop; let settled = false; let dropped = 0;
      const finish = (value, error) => {
        if (settled) return; settled = true; clearTimeout(timer);
        Promise.resolve(stop?.()).then(() => error ? reject(error) : resolve(value), reject);
      };
      const timer = setTimeout(() => finish({ observed: false }), timeout);
      api.watchWorkspaceEvents((batch) => {
        dropped = Math.min(Number.MAX_SAFE_INTEGER, dropped + Math.max(0, Number(batch?.dropped) || 0));
        const event = batch?.events?.find((candidate) =>
          (kind == null || candidate?.event === kind)
          && (slot == null || candidate?.slot === slot)
          && (phase == null || candidate?.phase === phase));
        if (event) finish({ observed: true, event, dropped });
      }).then((dispose) => { stop = dispose; if (settled) void dispose(); }, (error) => finish(undefined, error));
    }),
  ));
  if (typeof api.watchExecutions === 'function') definitions.push(define(
    'husklet_execution_change_wait',
    'Wait for a bounded execution catalogue change newer than an optional exact observed cursor.',
    z.object({ id, after: executionCursor.optional(), running: z.boolean().optional(), absent: z.boolean().default(false), timeout_ms: z.number().int().min(1).max(30_000).default(30_000) }).strict()
      .superRefine(({ after, running, absent }, context) => {
        if (absent && running != null) context.addIssue({ code: z.ZodIssueCode.custom, message: 'absent and running are mutually exclusive' });
        if (absent && after == null) context.addIssue({ code: z.ZodIssueCode.custom, message: 'execution removal waits require the full observed cursor' });
      }),
    ({ id: wanted, after, running, absent, timeout_ms: timeout }) => new Promise((resolve, reject) => {
      let stop; let settled = false;
      const finish = (value, error) => {
        if (settled) return; settled = true; clearTimeout(timer);
        Promise.resolve(stop?.()).then(() => error ? reject(error) : resolve(value), reject);
      };
      const timer = setTimeout(() => finish({ changed: false }), timeout);
      api.watchExecutions((catalogue) => {
        const execution = catalogue.executions.find(({ id: candidate }) => candidate === wanted);
        if (!execution) {
          if (absent) finish({ changed: true, execution: null, removed: true, truncated: catalogue.truncated });
          return;
        }
        const replaced = after != null && (execution.container_id !== after.container_id
          || execution.user !== after.user
          || JSON.stringify(execution.command) !== JSON.stringify(after.command));
        const unchanged = after != null && !replaced && execution.running === after.running
          && execution.exit_code === after.exit_code && execution.pid === after.pid;
        if (replaced || (!absent && !unchanged && (running == null || execution.running === running))) {
          finish({ changed: true, ...(after == null ? {} : { replaced }), ...(absent ? { removed: false } : {}), execution, truncated: catalogue.truncated });
        }
      }).then((dispose) => { stop = dispose; if (settled) void dispose(); }, (error) => finish(undefined, error));
    }),
  ));
  if (typeof api.watchContainers === 'function') definitions.push(define(
    'husklet_container_change_wait',
    'Wait for a bounded container snapshot newer than an optional observed state/creation cursor.',
    z.object({
      id,
      after: z.object({ state: z.string().min(1).max(64), created: z.number().int().nonnegative().safe() }).strict().optional(),
      state: z.string().min(1).max(64).optional(),
      absent: z.boolean().default(false),
      timeout_ms: z.number().int().min(1).max(30_000).default(30_000),
    }).strict()
      .refine(({ state, absent }) => !absent || state == null, 'absent and state are mutually exclusive'),
    ({ id: wanted, after, state, absent, timeout_ms: timeout }) => new Promise((resolve, reject) => {
      let stop; let settled = false;
      const finish = (value, error) => {
        if (settled) return; settled = true; clearTimeout(timer);
        Promise.resolve(stop?.()).then(() => error ? reject(error) : resolve(value), reject);
      };
      const timer = setTimeout(() => finish({ changed: false }), timeout);
      api.watchContainers((containers) => {
        const container = containers.find(({ id: candidate }) => candidate === wanted);
        const unchanged = after != null && container?.state === after.state && container?.created === after.created;
        if (!unchanged && ((absent && !container) || (!absent && container && (state == null || container.state === state)))) {
          finish({ changed: true, container: container ?? null });
        }
      }).then((dispose) => { stop = dispose; if (settled) void dispose(); }, (error) => finish(undefined, error));
    }),
  ));
  if (typeof api.watchImagePulls === 'function') definitions.push(define(
    'husklet_image_pull_wait',
    'Wait once for a revision of one exact image-pull job, then return its bounded full status without polling.',
    z.object({ job: imagePullJob, after_revision: z.number().int().nonnegative().safe().default(0), timeout_ms: z.number().int().min(1).max(30_000).default(30_000) }).strict(),
    ({ job, after_revision: after, timeout_ms: timeout }) => new Promise((resolve, reject) => {
      let stop; let settled = false;
      const finish = (value, error) => {
        if (settled) return; settled = true; clearTimeout(timer);
        Promise.resolve(stop?.()).then(() => error ? reject(error) : resolve(value), reject);
      };
      const timer = setTimeout(() => finish({ changed: false, job, after_revision: after }), timeout);
      api.watchImagePulls(async (change) => {
        if (change.job !== job || change.revision <= after || settled) return;
        try {
          const status = await api.images.pullStatus(job);
          if (status.job !== job) throw new Error(`host returned image pull job ${status.job}, expected ${job}`);
          finish({ changed: true, change, status });
        } catch (error) { finish(undefined, error); }
      }).then((dispose) => { stop = dispose; if (settled) void dispose(); }, (error) => finish(undefined, error));
    }),
  ));
  if (typeof api.watchExtensions === 'function' && typeof api.watchExtensionAcquisitions === 'function') definitions.push(define(
    'husklet_extension_wait',
    'Wait for a bounded installed-extension snapshot or acquisition revision invalidation without polling.',
    z.object({ kind: z.enum(['inventory', 'acquisition']), after: extensionInventoryCursor.optional(), job: extensionJob.optional(), after_revision: acquisitionRevision.optional(), timeout_ms: z.number().int().min(1).max(30_000).default(30_000) }).strict()
      .superRefine(({ kind, after, job, after_revision }, context) => {
        if ((job != null || after_revision != null) && kind !== 'acquisition') context.addIssue({ code: z.ZodIssueCode.custom, message: 'job and revision filtering apply only to acquisition changes' });
        if ((job == null) !== (after_revision == null)) context.addIssue({ code: z.ZodIssueCode.custom, message: 'job-specific acquisition waits require both job and after_revision' });
        if ((after != null) !== (kind === 'inventory')) context.addIssue({ code: z.ZodIssueCode.custom, message: 'installed-extension waits require an exact inventory cursor' });
      }),
    ({ kind, after, job, after_revision, timeout_ms: timeout }) => new Promise((resolve, reject) => {
      let stop; let settled = false;
      const finish = (value, error) => {
        if (settled) return; settled = true; clearTimeout(timer);
        Promise.resolve(stop?.()).then(() => error ? reject(error) : resolve(value), reject);
      };
      const timer = setTimeout(() => finish({ changed: false }), timeout);
      const watch = kind === 'inventory' ? api.watchExtensions : api.watchExtensionAcquisitions;
      watch((change) => {
        if (kind === 'inventory') {
          const current = change.find(({ name }) => name === after.name);
          if (current?.image_digest === after.image_digest && current.status === after.status) return;
          finish({ changed: true, change, extension: current ?? null, removed: current == null, replaced: current != null && current.image_digest !== after.image_digest });
        } else if (job == null || (change.job === job && change.revision > after_revision)) finish({ changed: true, change });
      })
        .then((dispose) => { stop = dispose; if (settled) void dispose(); }, (error) => finish(undefined, error));
    }),
  ));
  if (typeof api.watchWorkspaceLifecycle === 'function') definitions.push(define(
    'husklet_workspace_wait',
    'Wait for one bounded workspace lifecycle invalidation under WorkspaceRead authority.',
    z.object({
      workspace: id.optional(),
      action: z.enum(['create', 'update', 'remove', 'start', 'stop', 'restart']).optional(),
      timeout_ms: z.number().int().min(1).max(30_000).default(30_000),
    }).strict(),
    ({ workspace: wanted, action, timeout_ms: timeout }) => new Promise((resolve, reject) => {
      let stop; let settled = false;
      const finish = (value, error) => {
        if (settled) return; settled = true; clearTimeout(timer);
        Promise.resolve(stop?.()).then(() => error ? reject(error) : resolve(value), reject);
      };
      const timer = setTimeout(() => finish({ changed: false }), timeout);
      api.watchWorkspaceLifecycle((change) => {
        if ((wanted == null || change.workspace === wanted) && (action == null || change.action === action)) {
          finish({ changed: true, change });
        }
      }).then((dispose) => { stop = dispose; if (settled) void dispose(); }, (error) => finish(undefined, error));
    }),
  ));
  return definitions.concat(paneTools(api.terminal, api.watchPaneChanges?.bind(api)));
}

export function createServer(session) {
  const server = new McpServer({ name: '@husklet/mcp', version: '0.1.0' });
  for (const tool of tools(workspace(session))) {
    server.registerTool(tool.name, { description: tool.description, inputSchema: tool.inputSchema }, tool.run);
  }
  return server;
}
