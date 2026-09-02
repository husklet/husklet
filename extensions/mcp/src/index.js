import { McpServer } from '@modelcontextprotocol/sdk/server/mcp.js';
import { Buffer } from 'node:buffer';
import { z } from 'zod';
import { workspace } from '@husklet/react';
import { result } from './bounds.js';
import { paneTools } from './panes.js';
export { paneXml, semanticXml } from './panes.js';

const id = z.string().min(1).max(256);
const extensionName = z.string().min(1).max(128).regex(/^[A-Za-z0-9][A-Za-z0-9_.-]*$/);
const extensionJob = z.string().min(1).max(128);
const extensionCapability = z.enum(['workspace-read', 'workspace-control', 'workspace-events', 'container-read', 'container-control', 'image-read', 'image-write', 'volume-read', 'volume-write', 'network-read', 'network-write', 'terminal-read', 'terminal-control', 'terminal-output', 'pane-observe', 'pane-semantic-read', 'pane-semantic-control', 'extension-read', 'extension-control', 'extension-install', 'filesystem-read', 'filesystem-write', 'interface']);
const extensionGrant = z.array(extensionCapability).max(23);
const acquisitionRevision = z.number().int().nonnegative().safe();
const path = z.string().min(1).max(4096);
const containerName = z.string().min(1).max(128).regex(/^[A-Za-z0-9][A-Za-z0-9_.-]*$/);
const imageReference = z.string().min(1).max(512).refine((value) => value.trim() === value && !/\s/.test(value), 'image reference must not contain whitespace');
const command = z.array(z.string().max(4096)).min(1).max(64).superRefine((argv, context) => {
  if (argv.length > 0 && argv[0].length === 0) context.addIssue({ code: z.ZodIssueCode.custom, message: 'the executable must not be empty' });
  if (argv.some((argument) => argument.includes('\0'))) context.addIssue({ code: z.ZodIssueCode.custom, message: 'command arguments cannot contain NUL' });
  const bytes = argv.reduce((total, argument) => total + new TextEncoder().encode(argument).byteLength, 0);
  if (bytes > 32 * 1024) context.addIssue({ code: z.ZodIssueCode.custom, message: 'command exceeds 32768 bytes' });
});
const optionalCommand = z.array(z.string().max(4096)).max(64);
const containerCreate = z.object({
  image: imageReference,
  name: containerName,
  entrypoint: command.nullable().default(null),
  command: optionalCommand.default([]),
  environment: z.array(z.tuple([z.string().min(1).max(256).regex(/^[A-Za-z_][A-Za-z0-9_]*$/), z.string().max(8192)])).max(256).default([]),
  working_directory: z.string().min(1).max(4096).startsWith('/').nullable().default(null),
  user: z.string().min(1).max(256).nullable().default(null),
  labels: z.array(z.tuple([z.string().min(1).max(256), z.string().max(4096)])).max(128).default([]),
  mounts: z.array(z.object({ volume: containerName, target: z.string().min(1).max(4096).startsWith('/'), read_only: z.boolean().default(false) }).strict()).max(64).default([]),
  network: containerName.nullable().default(null),
  ports: z.array(z.object({ container: z.number().int().min(1).max(65535), host: z.number().int().min(1).max(65535).nullable().default(null), protocol: z.enum(['tcp', 'udp']) }).strict()).max(64).default([]),
  memory_mb: z.number().int().min(1).max(1_048_576).nullable().default(null),
  cpus: z.number().int().min(1).max(256).nullable().default(null),
  pids_limit: z.number().int().min(1).max(1_000_000).nullable().default(null),
}).strict().superRefine((spec, context) => {
  const issue = (message) => context.addIssue({ code: z.ZodIssueCode.custom, message });
  const argv = [...(spec.entrypoint ?? []), ...spec.command];
  if (spec.command.length > 0 && spec.command[0].length === 0) issue('command executable must not be empty');
  if (argv.some((argument) => argument.includes('\0'))
    || argv.reduce((total, argument) => total + new TextEncoder().encode(argument).byteLength, 0) > 32 * 1024) issue('entrypoint and command must be NUL-free and at most 32768 bytes');
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
const workspaceUpdate = z.object({ name: id, configuration: workspaceConfiguration, confirm: z.literal(true) }).strict()
  .superRefine(({ name, configuration }, context) => {
    if (name !== configuration.name) context.addIssue({ code: z.ZodIssueCode.custom, message: 'renaming a workspace is not supported' });
  });
const empty = z.object({}).strict();
const slot = z.object({ slot: id }).strict();
const define = (name, description, inputSchema, run) => ({ name, description, inputSchema, run: async (input) => result(await run(input)) });
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
const terminalBytes = z.object({
  slot: id,
  input_base64: z.string().min(4).max(BASE64_INPUT_CHARS).superRefine((encoded, context) => {
    try { decodeTerminalBytes(encoded); } catch (error) {
      context.addIssue({ code: z.ZodIssueCode.custom, message: error.message });
    }
  }),
}).strict();

export function tools(api) {
  const definitions = [
    define('husklet_workspace_info', 'Describe the hosting workspace.', empty, () => api.info()),
    define('husklet_workspace_list', 'List bounded workspace summaries.', empty, () => api.list()),
    define('husklet_workspace_inspect', 'Inspect one named workspace.', z.object({ name: id }).strict(), ({ name }) => api.inspect(name)),
    define('husklet_workspace_create', 'Create one workspace from a complete bounded configuration.', z.object({ configuration: workspaceConfiguration }).strict(), ({ configuration }) => api.create(configuration)),
    define('husklet_workspace_update', 'Replace a stopped workspace configuration after explicit confirmation; renaming is not supported.', workspaceUpdate, ({ name, configuration }) => api.update(name, configuration)),
    ...['start', 'stop', 'restart'].map((action) => define(`husklet_workspace_${action}`, `${action} a named workspace.`, z.object({ name: id }).strict(), async ({ name }) => { await api[action](name); return { done: true }; })),
    define('husklet_workspace_delete', 'Delete a stopped workspace after explicit confirmation.', z.object({ name: id, confirm: z.literal(true) }).strict(), async ({ name }) => { await api.delete(name); return { done: true }; }),
    define('husklet_extension_list', 'List bounded installed extension records and lifecycle status.', empty, () => api.extensions.list()),
    define('husklet_extension_inspect', 'Inspect one installed extension record.', z.object({ name: extensionName }).strict(), ({ name }) => api.extensions.inspect(name)),
    define('husklet_extension_enable', 'Persistently enable an installed extension after explicit confirmation.', z.object({ name: extensionName, confirm: z.literal(true) }).strict(), async ({ name }) => { await api.extensions.enable(name); return { done: true }; }),
    define('husklet_extension_disable', 'Persistently disable an installed extension after explicit confirmation.', z.object({ name: extensionName, confirm: z.literal(true) }).strict(), async ({ name }) => { await api.extensions.disable(name); return { done: true }; }),
    define('husklet_extension_remove', 'Forget one installed extension record after explicit confirmation; this does not install or pull images.', z.object({ name: extensionName, confirm: z.literal(true) }).strict(), async ({ name }) => { await api.extensions.remove(name); return { done: true }; }),
    define('husklet_extension_acquire', 'Start bounded asynchronous inspection of one image reference after explicit confirmation.', z.object({ reference: imageReference, confirm: z.literal(true) }).strict(), ({ reference }) => api.extensions.startAcquisition(reference)),
    define('husklet_extension_acquisition', 'Read bounded acquisition progress, digest, manifest identity, and requested grants.', z.object({ job: extensionJob }).strict(), ({ job }) => api.extensions.acquisition(job)),
    define('husklet_extension_acquisition_cancel', 'Cancel one acquisition after explicit confirmation.', z.object({ job: extensionJob, confirm: z.literal(true) }).strict(), async ({ job }) => { await api.extensions.cancelAcquisition(job); return { done: true }; }),
    define('husklet_extension_install', 'Consent and atomically install the observed revision of a ready digest-bound candidate.', z.object({ job: extensionJob, revision: acquisitionRevision, granted: extensionGrant, confirm: z.literal(true) }).strict(), ({ job, revision, granted }) => api.extensions.install(job, revision, granted)),
    define('husklet_extension_update', 'Consent and atomically replace an installed extension with the observed revision of a ready digest-bound candidate.', z.object({ job: extensionJob, revision: acquisitionRevision, granted: extensionGrant, confirm: z.literal(true) }).strict(), ({ job, revision, granted }) => api.extensions.update(job, revision, granted)),
    define('husklet_container_list', 'List containers.', empty, () => api.containers.list()),
    define('husklet_container_inspect', 'Inspect one container.', z.object({ id }).strict(), ({ id: value }) => api.containers.inspect(value)),
    define('husklet_container_processes', 'Read the bounded process table for one container.', z.object({ id }).strict(), ({ id: value }) => api.containers.processes(value)),
    define('husklet_container_execution', 'Inspect one bounded container execution.', z.object({ id }).strict(), ({ id: value }) => api.containers.execution(value)),
    define('husklet_execution_signal', 'Signal one execution without signaling its owning container.', z.object({ id, signal: z.string().min(1).max(32) }).strict(), async ({ id: value, signal }) => { await api.containers.signalExecution(value, signal); return { done: true }; }),
    define('husklet_container_logs', 'Read bounded container logs.', z.object({ id, stdout: z.boolean().default(true), stderr: z.boolean().default(true) }).strict(), ({ id: value, stdout, stderr }) => api.containers.logs(value, { stdout, stderr })),
    define('husklet_container_create', 'Create a bounded configured container from a local image; mounts are named volumes and published ports bind loopback only.', containerCreate, (spec) => api.containers.create(spec)),
    define('husklet_container_exec', 'Execute a bounded argv vector in a running container without shell parsing.', z.object({ id, command, user: z.string().min(1).max(256).optional(), working_directory: z.string().min(1).max(4096).startsWith('/').optional() }).strict(), ({ id: value, command: argv, user, working_directory: workingDirectory }) => api.containers.exec(value, { command: argv, user, workingDirectory })),
    ...['start', 'pause', 'unpause', 'restart'].map((action) => define(`husklet_container_${action}`, `${action} one container.`, z.object({ id }).strict(), async ({ id: value }) => { await api.containers[action](value); return { done: true }; })),
    define('husklet_container_stop', 'Stop one container after explicit confirmation.', z.object({ id, confirm: z.literal(true) }).strict(), async ({ id: value }) => { await api.containers.stop(value); return { done: true }; }),
    define('husklet_container_remove', 'Remove one container after explicit confirmation.', z.object({ id, confirm: z.literal(true) }).strict(), async ({ id: value }) => { await api.containers.remove(value); return { done: true }; }),
    define('husklet_container_kill', 'Signal one container after explicit confirmation; signal must be explicit.', z.object({ id, signal: z.string().min(1).max(32), confirm: z.literal(true) }).strict(), async ({ id: value, signal }) => { await api.containers.kill(value, signal); return { done: true }; }),
    define('husklet_volume_list', 'List bounded local volume summaries.', empty, () => api.volumes.list()),
    define('husklet_volume_inspect', 'Inspect one local volume.', z.object({ name: id }).strict(), ({ name }) => api.volumes.inspect(name)),
    define('husklet_volume_create', 'Create one named local volume.', z.object({ name: id }).strict(), ({ name }) => api.volumes.create(name)),
    define('husklet_volume_remove', 'Remove one volume after explicit confirmation.', z.object({ name: id, confirm: z.literal(true) }).strict(), async ({ name }) => { await api.volumes.remove(name); return { done: true }; }),
    define('husklet_network_list', 'List bounded local network summaries.', empty, () => api.networks.list()),
    define('husklet_network_inspect', 'Inspect one local network.', z.object({ reference: id }).strict(), ({ reference }) => api.networks.inspect(reference)),
    define('husklet_network_create', 'Create one named local network.', z.object({ name: id }).strict(), ({ name }) => api.networks.create(name)),
    define('husklet_network_remove', 'Remove one network after explicit confirmation.', z.object({ reference: id, confirm: z.literal(true) }).strict(), async ({ reference }) => { await api.networks.remove(reference); return { done: true }; }),
    define('husklet_network_connect', 'Connect one container to a network.', z.object({ reference: id, container: id }).strict(), async ({ reference, container }) => { await api.networks.connect(reference, container); return { done: true }; }),
    define('husklet_network_disconnect', 'Disconnect one container from a network after explicit confirmation.', z.object({ reference: id, container: id, confirm: z.literal(true) }).strict(), async ({ reference, container }) => { await api.networks.disconnect(reference, container); return { done: true }; }),
    define('husklet_image_list', 'List bounded local image summaries.', empty, () => api.images.list()),
    define('husklet_image_inspect', 'Inspect one local image.', z.object({ reference: id }).strict(), ({ reference }) => api.images.inspect(reference)),
    define('husklet_image_pull', 'Pull one explicit image reference.', z.object({ reference: id }).strict(), ({ reference }) => api.images.pull(reference)),
    define('husklet_image_remove', 'Remove one image after explicit confirmation.', z.object({ reference: id, confirm: z.literal(true) }).strict(), async ({ reference }) => { await api.images.remove(reference); return { done: true }; }),
    define('husklet_image_prune', 'Prune unused images after explicit confirmation.', z.object({ confirm: z.literal(true) }).strict(), () => api.images.prune()),
    define('husklet_terminal_tabs', 'List terminal tabs.', empty, () => api.terminal.tabs()),
    define('husklet_terminal_topology', 'Read terminal split topology.', empty, () => api.terminal.topology()),
    define('husklet_terminal_read', 'Read at most 500 lines from one pane.', z.object({ slot: id, lines: z.number().int().min(1).max(500) }).strict(), ({ slot: value, lines }) => api.terminal.read(value, lines)),
    define('husklet_terminal_write', 'Write bounded literal input to a pane; this does not spawn a shell command.', z.object({ slot: id, input: z.string().max(8192) }).strict(), async ({ slot: value, input }) => { await api.terminal.writeInput(value, input); return { done: true }; }),
    define('husklet_terminal_write_bytes', 'Write up to 65536 arbitrary bytes from canonical padded base64, including control and non-UTF8 bytes.', terminalBytes, async ({ slot: value, input_base64: encoded }) => { await api.terminal.writeInput(value, decodeTerminalBytes(encoded)); return { done: true }; }),
    define('husklet_terminal_open', 'Open a terminal tab.', z.object({ title: z.string().max(256).optional() }).strict(), ({ title }) => api.terminal.openTab(title ?? null)),
    define('husklet_terminal_split', 'Split a pane beside or below the selected pane.', z.object({ slot: id, division: z.enum(['beside', 'below']) }).strict(), ({ slot: value, division }) => api.terminal.split(value, division)),
    define('husklet_terminal_spawn', 'Replace one terminal pane process with a bounded exact argv vector; no shell parsing.', z.object({ slot: id, command }).strict(), async ({ slot: value, command: argv }) => { await api.terminal.spawn(value, argv); return { done: true }; }),
    define('husklet_terminal_focus', 'Focus one pane.', slot, async ({ slot: value }) => { await api.terminal.focus(value); return { done: true }; }),
    define('husklet_terminal_resize', 'Request a bounded terminal grid size.', z.object({ slot: id, columns: z.number().int().min(1).max(1000), rows: z.number().int().min(1).max(1000) }).strict(), async ({ slot: value, columns, rows }) => { await api.terminal.resizeGrid(value, columns, rows); return { done: true }; }),
    define('husklet_terminal_ratio', 'Set the pane share of its split.', z.object({ slot: id, ratio: z.number().min(0.05).max(0.95) }).strict(), async ({ slot: value, ratio }) => { await api.terminal.ratio(value, ratio); return { done: true }; }),
    define('husklet_terminal_close', 'Close one pane after explicit confirmation.', z.object({ slot: id, confirm: z.literal(true) }).strict(), async ({ slot: value }) => { await api.terminal.close(value); return { done: true }; }),
    define('husklet_file_list', 'List a workspace-relative directory.', z.object({ path }).strict(), ({ path: value }) => api.files.list(value)),
    define('husklet_file_read', 'Read one bounded workspace-relative file.', z.object({ path }).strict(), ({ path: value }) => api.files.read(value)),
    define('husklet_file_write', 'Write bounded UTF-8 contents to a workspace-relative file.', z.object({ path, contents: z.string().max(64 * 1024) }).strict(), async ({ path: value, contents }) => { await api.files.write(value, new TextEncoder().encode(contents)); return { done: true }; }),
    define('husklet_file_mkdir', 'Create one workspace-relative directory.', z.object({ path }).strict(), async ({ path: value }) => { await api.files.mkdir(value); return { done: true }; }),
    define('husklet_file_rename', 'Rename one workspace-relative entry without overwriting.', z.object({ from: path, to: path }).strict(), async ({ from, to }) => { await api.files.rename(from, to); return { done: true }; }),
    define('husklet_file_remove', 'Remove one file or empty directory after explicit confirmation.', z.object({ path, confirm: z.literal(true) }).strict(), async ({ path: value }) => { await api.files.remove(value); return { done: true }; }),
  ];
  if (typeof api.terminal?.panes === 'function') definitions.push(define(
    'husklet_pane_list',
    'List every inspectable terminal, extension surface, and native pane without reading its contents.',
    empty,
    () => api.terminal.panes(),
  ));
  if (typeof api.watchPaneChanges === 'function') definitions.push(define(
    'husklet_pane_wait',
    'Wait for bounded pane-change metadata; fetch a snapshot after notification.',
    z.object({ slot: id.optional(), timeout_ms: z.number().int().min(1).max(30_000).default(30_000) }).strict(),
    ({ slot: wanted, timeout_ms: timeout }) => new Promise((resolve, reject) => {
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
        if (wanted == null || change.slot === wanted) finish({ changed: true, change });
      }).then((dispose) => { stop = dispose; if (settled) void dispose(); }, (error) => finish(undefined, error));
    }),
  ));
  if (typeof api.watchExtensions === 'function' && typeof api.watchExtensionAcquisitions === 'function') definitions.push(define(
    'husklet_extension_wait',
    'Wait for a bounded installed-extension snapshot or acquisition revision invalidation without polling.',
    z.object({ kind: z.enum(['inventory', 'acquisition']), job: extensionJob.optional(), timeout_ms: z.number().int().min(1).max(30_000).default(30_000) }).strict()
      .superRefine(({ kind, job }, context) => { if (job != null && kind !== 'acquisition') context.addIssue({ code: z.ZodIssueCode.custom, message: 'job filtering applies only to acquisition changes' }); }),
    ({ kind, job, timeout_ms: timeout }) => new Promise((resolve, reject) => {
      let stop; let settled = false;
      const finish = (value, error) => {
        if (settled) return; settled = true; clearTimeout(timer);
        Promise.resolve(stop?.()).then(() => error ? reject(error) : resolve(value), reject);
      };
      const timer = setTimeout(() => finish({ changed: false }), timeout);
      const watch = kind === 'inventory' ? api.watchExtensions : api.watchExtensionAcquisitions;
      watch((change) => { if (kind === 'inventory' || job == null || change.job === job) finish({ changed: true, change }); })
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
  return definitions.concat(paneTools(api.terminal));
}

export function createServer(session) {
  const server = new McpServer({ name: '@husklet/mcp', version: '0.1.0' });
  for (const tool of tools(workspace(session))) {
    server.registerTool(tool.name, { description: tool.description, inputSchema: tool.inputSchema }, tool.run);
  }
  return server;
}
