import React from 'react';
import {
  Badge, Button, Card, CardActions, CardContent, CardHeader, Column, Entry,
  ConfirmAction, EmptyState, Heading, KeyValueTable, List, ListItemButton, LogView, Meter, ObjectInspector, Progress, ResourceState, Row, Scroll, Separator, Spinner, Text,
  LOG_VIEW_CHARACTER_LIMIT,
} from '@husklet/react';
import { ContainerDetailsSource, EXECUTION_DETAIL_SOURCE, ExecutionDetailsSource, ImageDetailsSource, NetworkDetailsSource, VolumeDetailsSource, LOG_LIMIT, bounded, boundedMessage, bytes, containerNameError, endpointAliases, immutableContainerId, logText, processRows, resourceReference, shortId } from './model.js';

const { createElement: h, useCallback, useEffect, useMemo, useRef, useState } = React;
const SECTIONS = ['overview', 'containers', 'processes', 'executions', 'images', 'volumes', 'networks'];
const INSPECTOR_BOUNDS = Object.freeze({ maxDepth: 8, maxNodes: 128, maxStringLength: 256 });

function StructuredDetail({ value }) {
  return h(ObjectInspector, { value, ...INSPECTOR_BOUNDS, height: { minimum: { step: 10 }, maximum: { step: 32 } } });
}

export function WorkspaceManager({ api, selections, containerDetails, executionDetails, imageDetails, networkDetails, volumeDetails, initial = {} }) {
  const [section, setSection] = useState('overview');
  const [requestedExecution, setRequestedExecution] = useState('');
  const containers = useResource(api.containers.list, initial.containers);
  const images = useResource(api.images.list, initial.images);
  const volumes = useResource(api.volumes.list, initial.volumes);
  const networks = useResource(api.networks.list, initial.networks);
  const [executionsTruncated, setExecutionsTruncated] = useState(false);
  const listExecutions = useCallback(async () => {
    const listing = await api.containers.executions();
    setExecutionsTruncated(listing.truncated);
    return listing.executions;
  }, [api]);
  const executions = useResource(listExecutions, initial.executions);
  useEffect(() => {
    if (section !== 'executions' || typeof api.watchExecutions !== 'function') return undefined;
    let disposed = false;
    let stop = null;
    void api.watchExecutions((listing) => {
      if (disposed) return;
      setExecutionsTruncated(listing.truncated);
      executions.replace(listing.executions);
    }).then((dispose) => {
      if (disposed) void dispose();
      else stop = dispose;
    }).catch(() => { /* Explicit Refresh remains available when observation is unsupported. */ });
    return () => {
      disposed = true;
      if (stop) void stop();
    };
  }, [api, section, executions.replace]);
  useEffect(() => selections?.subscribe((event) => {
    if (SECTIONS.includes(event?.pane_provider)) setSection(event.pane_provider);
    if (event?.snapshot === 'containers') void containers.reload();
    if (event?.snapshot === 'images') void images.reload();
    if (event?.snapshot === 'volumes') void volumes.reload();
    if (event?.snapshot === 'networks') void networks.reload();
  }), [selections, containers.reload, images.reload, volumes.reload, networks.reload]);
  useEffect(() => {
    if (typeof api.subscribe !== 'function') return undefined;
    void api.subscribe('containers');
    void api.subscribe('images');
    void api.subscribe('volumes');
    void api.subscribe('networks');
    return () => {
      if (typeof api.unsubscribe === 'function') {
        void api.unsubscribe('containers');
        void api.unsubscribe('images');
        void api.unsubscribe('volumes');
        void api.unsubscribe('networks');
      }
    };
  }, [api]);
  const body = section === 'overview'
    ? h(Overview, { containers, images, volumes, networks, onOpen: setSection })
    : section === 'containers'
      ? h(Containers, { api, resource: containers, containerDetails, onOpenExecution: async (id) => {
        setRequestedExecution(id);
        await executions.reload();
        setSection('executions');
      } })
      : section === 'processes'
        ? h(Processes, { api, resource: containers })
        : section === 'executions'
          ? h(Executions, { api, resource: executions, executionDetails, truncated: executionsTruncated, requestedExecution })
        : section === 'images'
          ? h(Images, { api, resource: images, imageDetails })
          : section === 'volumes'
            ? h(Volumes, { api, resource: volumes, volumeDetails })
            : h(Networks, { api, resource: networks, networkDetails });
  return h(Row, { grow: true, gap: 0 }, h(Navigation, { section, onSelect: setSection }), h(Separator, { orientation: 'vertical' }), body);
}

function Navigation({ section, onSelect }) {
  return h(Column, { width: { chars: 22 }, height: 'fill', pad: 2, gap: 1 },
    h(Heading, { label: 'Workspace', scale: 'title' }),
    h(Text, { label: 'Runtime resources', color: 'text-dim' }),
    h(List, { grow: true }, ...SECTIONS.map((name) => h(ListItemButton, {
      key: name, label: title(name), selected: section === name, onInvoke: () => onSelect(name),
    }))));
}

function Overview({ containers, images, volumes, networks, onOpen }) {
  const running = (containers.data ?? []).filter((item) => item.state === 'running').length;
  return h(Scroll, { grow: true, height: 'fill' }, h(Column, { pad: 4, gap: 3 },
    h(Heading, { label: 'Workspace overview', scale: 'title' }),
    h(Text, { label: 'Inspect and operate the resources in this workspace.', color: 'text-dim' }),
    h(Row, { gap: 2, wrap: true },
      h(Summary, { title: 'Containers', value: containers.loading ? '…' : String((containers.data ?? []).length), detail: `${running} running`, onOpen: () => onOpen('containers') }),
      h(Summary, { title: 'Images', value: images.loading ? '…' : String((images.data ?? []).length), detail: 'Available locally', onOpen: () => onOpen('images') }),
      h(Summary, { title: 'Volumes', value: volumes.loading ? '…' : String((volumes.data ?? []).length), detail: 'Durable local storage', onOpen: () => onOpen('volumes') }),
      h(Summary, { title: 'Networks', value: networks.loading ? '…' : String((networks.data ?? []).length), detail: 'Workspace-local connectivity', onOpen: () => onOpen('networks') })),
    h(ErrorText, { error: containers.error ?? images.error ?? volumes.error ?? networks.error })));
}

function Summary({ title: label, value, detail, onOpen }) {
  return h(Card, { width: { minimum: { chars: 24 } }, variant: 'outline' },
    h(CardHeader, { label }), h(CardContent, { gap: 1 }, h(Heading, { label: value, scale: 'title' }), h(Text, { label: detail, color: 'text-dim' })),
    h(CardActions, {}, h(Button, { label: 'Open', variant: 'ghost', onInvoke: onOpen })));
}

export function Containers({ api, resource, containerDetails, onOpenExecution }) {
  const localDetails = useMemo(() => new ContainerDetailsSource(), []);
  const detailsSource = containerDetails ?? localDetails;
  const [selected, setSelected] = useState(null);
  const [busy, setBusy] = useState('');
  const [inspection, setInspection] = useState({ id: '', state: 'idle', count: 0, detail: null, error: null });
  const [draft, setDraft] = useState({ image: '', name: '' });
  const [created, setCreated] = useState(null);
  const [creationError, setCreationError] = useState(null);
  const [creationNotice, setCreationNotice] = useState('');
  const act = async (verb, id, ...args) => {
    setBusy(`${verb}:${id}`);
    try { await api.containers[verb](id, ...args); await resource.reload(); } finally { setBusy(''); }
  };
  const inspect = async (item) => {
    setSelected(item.id);
    setInspection({ id: item.id, state: 'loading', count: 0, detail: null, error: null });
    try {
      const detail = await api.containers.inspect(item.id);
      const count = await detailsSource.replace(detail);
      setInspection({ id: item.id, state: 'ready', count, detail, error: null });
    } catch (cause) {
      setInspection({ id: item.id, state: 'error', count: 0, detail: null, error: cause });
    }
  };
  const toggleDetails = (item) => {
    if (selected === item.id && inspection.state !== 'error') {
      setSelected(null);
      return;
    }
    void inspect(item);
  };
  const createAndStart = async () => {
    setBusy('create'); setCreationError(null); setCreationNotice('');
    let target = created;
    try {
      if (!target) {
        const id = await api.containers.create({ image: draft.image.trim(), name: draft.name.trim() });
        target = { id, name: draft.name.trim() };
        setCreated(target);
      }
      await api.containers.start(target.id);
      setCreationNotice(`Created and started ${target.name}.`);
      setCreated(null); setDraft({ image: '', name: '' });
      await resource.reload();
    } catch (cause) { setCreationError(cause); } finally { setBusy(''); }
  };
  const view = bounded(resource.data);
  const state = resource.loading ? 'loading' : resource.error ? 'error' : view.records.length === 0 ? 'empty' : 'ready';
  return h(Page, { title: 'Containers', subtitle: 'Lifecycle, process inspection, logs, and execution.' },
    h(Card, { variant: 'outline' },
      h(CardHeader, { label: 'Create a container', detail: 'Uses a local image and starts it after durable creation.' }),
      h(CardContent, {}, h(Row, { gap: 1, wrap: true },
        h(Entry, { value: draft.image, placeholder: 'Image reference', enabled: !created && busy !== 'create', onChange: (event) => setDraft((value) => ({ ...value, image: String(event.value ?? '') })) }),
        h(Entry, { value: draft.name, placeholder: 'Container name', enabled: !created && busy !== 'create', onChange: (event) => setDraft((value) => ({ ...value, name: String(event.value ?? '') })) }))),
      h(CardActions, {}, busy === 'create' ? h(Spinner) : null, h(Button, {
        label: created ? 'Retry start' : busy === 'create' ? 'Creating…' : 'Create and start',
        enabled: busy === '' && (created !== null || (draft.image.trim().length > 0 && draft.name.trim().length > 0)),
        onInvoke: createAndStart,
      })),
      h(ErrorText, { error: creationError }), creationNotice ? h(Text, { label: creationNotice, color: 'positive', wrap: true }) : null),
    h(Toolbar, { loading: resource.loading, onRefresh: resource.reload }),
    h(ResourceState, {
      state,
      loadingLabel: 'Reading containers…',
      emptyLabel: 'No containers',
      emptyDetail: 'Create a container through an agent or extension, then refresh this page.',
      error: resource.error?.message ?? String(resource.error ?? ''),
      retryLabel: 'Retry containers',
      onRetry: resource.reload,
    }, ...view.records.map((item) => h(Card, { key: item.id, variant: selected === item.id ? 'filled' : 'outline' },
      h(CardHeader, { label: item.name || shortId(item.id), detail: item.image }),
      h(CardContent, { gap: 2 },
        h(Row, { gap: 2, align: 'center' }, h(Badge, { label: item.state, tone: stateTone(item.state) }), h(Text, { label: shortId(item.id), color: 'text-dim' })),
        h(ContainerRename, { api, container: item, reload: resource.reload, blocked: busy !== '' })),
      h(CardActions, { gap: 1 },
        h(Button, { label: selected === item.id ? 'Hide details' : 'Details', onInvoke: () => toggleDetails(item) }),
        ...containerActions(item, busy, act)),
      selected === item.id ? h(ContainerDetail, { api, container: item, act, inspection, onRetry: () => inspect(item), onOpenExecution }) : null)),
    h(Omitted, { count: view.omitted })));
}

function ContainerRename({ api, container, reload, blocked }) {
  const current = container.name ?? '';
  const [draft, setDraft] = useState(current);
  const [result, setResult] = useState({ state: 'idle', error: null, name: '' });
  useEffect(() => {
    setDraft(current);
    setResult({ state: 'idle', error: null, name: '' });
  }, [container.id, current]);
  const validation = containerNameError(draft);
  const rename = async () => {
    if (validation || draft === current || result.state === 'loading') return;
    const requested = draft;
    const immutableId = container.id;
    setResult({ state: 'loading', error: null, name: requested });
    try {
      await api.containers.rename(immutableId, requested);
      setResult({ state: 'success', error: null, name: requested });
      await reload();
    } catch (error) {
      setResult({ state: 'error', error, name: requested });
    }
  };
  const changed = draft !== current;
  return h(Column, { gap: 1 },
    h(Heading, { label: 'Rename container', scale: 'caption' }),
    h(Text, { label: `Current name: ${current || '(unnamed)'}. Immutable ID: ${container.id}`, color: 'text-dim', wrap: true }),
    h(Row, { gap: 1, wrap: true, align: 'center' },
      h(Entry, {
        value: draft, placeholder: `New name for ${shortId(container.id)}`, enabled: !blocked && result.state !== 'loading',
        onChange: (event) => {
          setDraft(String(event.value ?? '').slice(0, 129));
          setResult({ state: 'idle', error: null, name: '' });
        },
      }),
      result.state === 'loading' ? h(Spinner) : null,
      h(Button, {
        label: result.state === 'loading' ? 'Renaming…' : result.state === 'error' ? 'Retry rename' : 'Rename',
        enabled: !blocked && result.state !== 'loading' && changed && !validation,
        onInvoke: rename,
      })),
    changed && validation ? h(Text, { label: validation, color: 'danger', wrap: true }) : null,
    result.state === 'error' ? h(Text, { label: result.error?.message ?? String(result.error), color: 'danger', wrap: true }) : null,
    result.state === 'success' ? h(Text, {
      label: `Renamed to ${result.name}. Inventory identity will update after the authoritative refresh.`, color: 'positive', wrap: true,
    }) : null);
}

function containerActions(item, busy, act) {
  const blocked = busy !== '';
  const running = item.state === 'running';
  return [
    h(Button, { key: 'start', label: running ? 'Restart' : 'Start', enabled: !blocked, onInvoke: () => act(running ? 'restart' : 'start', item.id) }),
    h(Button, { key: 'pause', label: item.state === 'paused' ? 'Resume' : 'Pause', enabled: !blocked && (running || item.state === 'paused'), onInvoke: () => act(item.state === 'paused' ? 'unpause' : 'pause', item.id) }),
    h(ConfirmAction, {
      key: 'stop', label: 'Stop', confirmLabel: 'Confirm stop', pendingLabel: 'Confirm stop',
      authorityKey: `container:${item.id}:stop`,
      question: `Stop ${item.name || shortId(item.id)} with immutable ID ${item.id}?`, enabled: !blocked && running,
      onConfirm: () => act('stop', item.id),
    }),
  ];
}

function ContainerDetail({ api, container, act, inspection, onRetry, onOpenExecution }) {
  const [command, setCommand] = useState({ argv: '', user: '', workingDirectory: '' });
  const [execution, setExecution] = useState({ state: 'idle', id: '', error: null });
  const [attachment, setAttachment] = useState({ state: 'idle', slot: '', error: null });
  const [logs, setLogs] = useState(null);
  const run = async () => {
    setExecution({ state: 'loading', id: '', error: null });
    try {
      let argv;
      try { argv = JSON.parse(command.argv); }
      catch { throw new Error('Command must be valid JSON, such as ["sh","-lc","printf hello"].'); }
      if (!Array.isArray(argv) || argv.length === 0 || argv.length > 64
        || argv.some((argument) => typeof argument !== 'string' || argument.length > 4_096)) {
        throw new Error('Command must be a JSON array of 1–64 strings, each at most 4096 characters.');
      }
      if (command.user.length > 4_096 || command.workingDirectory.length > 4_096) {
        throw new Error('User and working directory must each be at most 4096 characters.');
      }
      const id = await api.containers.exec(container.id, {
        command: argv,
        ...(command.user.trim() ? { user: command.user.trim() } : {}),
        ...(command.workingDirectory.trim() ? { workingDirectory: command.workingDirectory.trim() } : {}),
      });
      setExecution({ state: 'ready', id, error: null });
    } catch (error) { setExecution({ state: 'error', id: '', error }); }
  };
  const readLogs = async () => setLogs(logText(await api.containers.logs(container.id, { stdout: true, stderr: true })).slice(-LOG_LIMIT * 160));
  const attach = async () => {
    setAttachment({ state: 'loading', slot: '', error: null });
    try {
      let argv;
      try { argv = JSON.parse(command.argv); }
      catch { throw new Error('Command must be valid JSON, such as ["sh"].'); }
      if (!Array.isArray(argv) || argv.length === 0 || argv.length > 64
        || argv.some((argument) => typeof argument !== 'string' || !argument.length || argument.length > 4_096)) {
        throw new Error('Command must be a JSON array of 1–64 non-empty strings, each at most 4096 characters.');
      }
      const slot = await api.containers.attachTerminal(container.id, argv);
      setAttachment({ state: 'ready', slot, error: null });
    } catch (error) { setAttachment({ state: 'error', slot: '', error }); }
  };
  return h(CardContent, { gap: 2 },
    h(ResourceState, {
      state: inspection.state === 'idle' ? 'loading' : inspection.state === 'ready' && inspection.count === 0 ? 'empty' : inspection.state,
      loadingLabel: 'Reading container details…',
      emptyLabel: 'No container details',
      emptyDetail: 'The host returned no inspectable fields.',
      error: inspection.error?.message ?? String(inspection.error ?? ''),
      retryLabel: 'Retry details',
      onRetry,
    }, h(StructuredDetail, { value: inspection.detail })),
    h(Separator), h(Heading, { label: 'Quick actions', scale: 'caption' }),
    h(Row, { gap: 1, wrap: true }, h(Button, { label: 'Load logs', onInvoke: readLogs }), h(ConfirmAction, {
      authorityKey: `container:${container.id}:kill:SIGKILL`,
      label: 'Kill', confirmLabel: 'Confirm kill', pendingLabel: 'Confirm kill', question: `Force-kill ${container.name || shortId(container.id)} with immutable ID ${container.id}?`,
      onConfirm: () => act('kill', container.id, 'SIGKILL'),
    })),
    logs === null ? null : h(Text, { label: logs || 'No log output.', wrap: true }),
    h(Separator), h(Heading, { label: 'Captured execution', scale: 'caption' }),
    h(Text, { label: 'Runs without an interactive terminal. Inspect the resulting record for status and captured stdout/stderr.', color: 'text-dim', wrap: true }),
    h(Text, { label: 'Enter an argument array so spaces and quoting remain exact, for example ["sh","-lc","printf hello"].', color: 'text-dim', wrap: true }),
    h(Row, { gap: 1, wrap: true },
      h(Entry, { value: command.argv, placeholder: 'Command argv JSON', enabled: execution.state !== 'loading', onChange: (event) => setCommand((value) => ({ ...value, argv: String(event.value ?? '') })) }),
      h(Entry, { value: command.user, placeholder: 'Run as user (optional)', enabled: execution.state !== 'loading', onChange: (event) => setCommand((value) => ({ ...value, user: String(event.value ?? '') })) }),
      h(Entry, { value: command.workingDirectory, placeholder: 'Working directory (optional)', enabled: execution.state !== 'loading', onChange: (event) => setCommand((value) => ({ ...value, workingDirectory: String(event.value ?? '') })) })),
    h(Row, { gap: 1, wrap: true },
      h(Button, { label: execution.state === 'loading' ? 'Executing…' : 'Execute', enabled: execution.state !== 'loading' && command.argv.trim().length > 0, onInvoke: run }),
      h(Button, { label: attachment.state === 'loading' ? 'Attaching…' : 'Attach terminal', enabled: attachment.state !== 'loading' && command.argv.trim().length > 0 && container.state === 'running', onInvoke: attach })),
    attachment.state === 'error' ? h(Text, { label: attachment.error?.message ?? String(attachment.error), color: 'danger', wrap: true }) : null,
    attachment.state === 'ready' ? h(Text, { label: `Interactive terminal opened in ${attachment.slot}.`, color: 'positive', wrap: true }) : null,
    execution.state === 'error' ? h(Text, { label: execution.error?.message ?? String(execution.error), color: 'danger', wrap: true }) : null,
    execution.state === 'ready' ? h(Row, { gap: 1, wrap: true, align: 'center' },
      h(Text, { label: `Execution ${execution.id} created.`, color: 'positive', wrap: true }),
      onOpenExecution ? h(Button, { label: 'Inspect execution', onInvoke: () => onOpenExecution(execution.id) }) : null) : null);
}

export function Processes({ api, resource }) {
  const [snapshots, setSnapshots] = useState([]);
  const [error, setError] = useState(null);
  const load = useCallback(async () => {
    try {
      const groups = await Promise.all((resource.data ?? []).map(async (container) => ({ container, rows: await api.containers.processes(container.id) })));
      setSnapshots(groups);
      setError(null);
    } catch (cause) { setError(cause); }
  }, [api, resource.data]);
  useEffect(() => { void load(); }, [load]);
  const processes = snapshots.flatMap(({ container, rows }) => processRows(rows, container.name || shortId(container.id)));
  const observed = Math.max(0, ...snapshots.map(({ rows }) => Number(rows.observed_at_ms) || 0));
  const view = bounded(processes);
  return h(Page, { title: 'Processes', subtitle: 'A bounded snapshot across all visible containers.' },
    h(Toolbar, { loading: resource.loading, onRefresh: load }), h(ErrorText, { error }),
    h(InventoryEmpty, { resource: { loading: resource.loading, error }, records: view.records, label: 'No running processes', detail: 'Start a container to see its process snapshot here.' }),
    h(Text, { label: 'Initial processes only; PIDs identify this snapshot and may be reused.', color: 'text-dim', wrap: true }),
    observed > 0 ? h(Text, { label: `Observed ${new Date(observed).toISOString()}`, color: 'text-dim' }) : null,
    ...view.records.map((process, index) => {
      const pid = process.cells.PID ?? process.cells.Pid ?? process.cells.pid ?? '—';
      const command = process.cells.CMD ?? process.cells.Command ?? process.cells.COMMAND ?? process.values.at(-1) ?? 'Process';
      const detail = Object.entries(process.cells).filter(([key]) => !['PID', 'Pid', 'pid', 'CMD', 'Command', 'COMMAND'].includes(key)).map(([key, value]) => `${key} ${value}`).join(' · ');
      return h(Card, { key: `${process.container}:${pid}:${index}`, variant: 'outline' },
        h(CardHeader, { label: command, detail: process.container }),
        h(CardContent, {}, h(Row, { gap: 2 }, h(Badge, { label: `PID ${pid}` }), h(Text, { label: detail, color: 'text-dim' }))));
    }),
    h(Omitted, { count: view.omitted }),
    snapshots.some(({ rows }) => rows.truncated)
      ? h(Text, { label: 'The host process snapshot was truncated at its safety limit.', color: 'warning', wrap: true }) : null);
}

export function Executions({ api, resource, executionDetails, truncated = false, requestedExecution = '' }) {
  const localDetails = useMemo(() => new ExecutionDetailsSource(), []);
  const detailsSource = executionDetails ?? localDetails;
  const [selected, setSelected] = useState('');
  const [inspection, setInspection] = useState({ state: 'idle', count: 0, error: null });
  const [output, setOutput] = useState(null);
  const [busy, setBusy] = useState('');
  const inspect = async (id) => {
    setSelected(id); setInspection({ state: 'loading', count: 0, error: null }); setOutput(null);
    try {
      const detail = await api.containers.execution(id);
      const count = await detailsSource.replace(detail);
      setInspection({ state: 'ready', count, error: null });
    } catch (error) { setInspection({ state: 'error', count: 0, error }); }
  };
  useEffect(() => {
    if (requestedExecution && selected !== requestedExecution) void inspect(requestedExecution);
  }, [requestedExecution]);
  const logs = async (id) => {
    setBusy(`logs:${id}`);
    try {
      const value = await api.containers.executionLogs(id, { stdout: true, stderr: true });
      const text = (bytes) => logText(bytes).slice(-LOG_VIEW_CHARACTER_LIMIT);
      setOutput((current) => ({ revision: (current?.revision ?? 0) + 1,
        stdout: text({ stdout: value.stdout, stderr: [] }), stderr: text({ stdout: [], stderr: value.stderr }),
        truncated: value.truncated, stdoutTruncated: value.stdout_truncated, stderrTruncated: value.stderr_truncated,
        eof: value.eof }));
    } finally { setBusy(''); }
  };
  const wait = async (id) => {
    setBusy(`wait:${id}`);
    try { const detail = await api.containers.waitExecution(id, { timeoutMs: 5_000 }); await detailsSource.replace(detail); await resource.reload(); }
    finally { setBusy(''); }
  };
  const terminate = async (id) => {
    await api.containers.signalExecution(id, 'SIGTERM');
    await resource.reload();
    await inspect(id);
  };
  const remove = async (id) => { await api.containers.removeExecution(id); setSelected(''); setOutput(null); await resource.reload(); };
  const view = bounded(resource.data);
  return h(Page, { title: 'Executions', subtitle: 'Bounded exec-session catalogue, status and captured output.' },
    h(Toolbar, { loading: resource.loading, onRefresh: resource.reload }), h(ErrorText, { error: resource.error }),
    h(InventoryEmpty, { resource, records: view.records, label: 'No executions', detail: 'Commands executed in containers will appear here.' }),
    ...view.records.map((item) => h(Card, { key: item.id, variant: selected === item.id ? 'filled' : 'outline' },
      h(CardHeader, { label: item.command?.join(' ') || shortId(item.id), detail: `container ${shortId(item.container_id)}` }),
      h(CardContent, {}, h(Badge, { label: item.running ? 'running' : `exited ${item.exit_code}`, tone: item.running ? 'positive' : 'neutral' }),
        selected !== item.id ? null : h(ResourceState, {
          state: inspection.state === 'idle' ? 'loading' : inspection.state === 'ready' && inspection.count === 0 ? 'empty' : inspection.state,
          loadingLabel: 'Reading execution details…',
          emptyLabel: 'No execution details',
          emptyDetail: 'The host returned no inspectable fields.',
          error: inspection.error?.message ?? String(inspection.error ?? ''),
          retryLabel: 'Retry details',
          onRetry: () => inspect(item.id),
        }, h(KeyValueTable, { source: EXECUTION_DETAIL_SOURCE, schema: IMAGE_DETAIL_SCHEMA, height: { minimum: { step: 10 }, maximum: { step: 28 } } })),
        selected === item.id && output ? h(Column, { gap: 1 },
          h(Heading, { label: 'Standard output', scale: 'caption' }), h(LogView, { key: `stdout-${output.revision}`, value: output.stdout || (output.eof ? 'No stdout captured (EOF).' : 'No stdout captured yet; execution is still running.'), monospace: true }),
          output.stdoutTruncated ? h(Text, { label: 'Standard output was truncated to its configured bound.', color: 'warning' }) : null,
          h(Heading, { label: 'Standard error', scale: 'caption' }), h(LogView, { key: `stderr-${output.revision}`, value: output.stderr || (output.eof ? 'No stderr captured (EOF).' : 'No stderr captured yet; execution is still running.'), monospace: true }),
          output.stderrTruncated ? h(Text, { label: 'Standard error was truncated to its configured bound.', color: 'warning' }) : null,
          output.eof ? h(Text, { label: 'Captured output is complete (EOF).', color: 'text-dim' })
            : h(Text, { label: 'Execution is still running; later output may appear.', color: 'text-dim' }),
          output.truncated && !output.stdoutTruncated && !output.stderrTruncated
            ? h(Text, { label: 'Host output was truncated to its configured bound.', color: 'warning' }) : null) : null),
      h(CardActions, { gap: 1 },
        h(Button, { label: selected === item.id ? 'Hide details' : 'Details', enabled: !busy, onInvoke: () => selected === item.id ? setSelected('') : void inspect(item.id) }),
        h(Button, { label: busy === `logs:${item.id}` ? 'Loading logs…' : 'Load output', enabled: !busy, onInvoke: () => void logs(item.id) }),
        h(Button, { label: busy === `wait:${item.id}` ? 'Waiting…' : 'Wait up to 5s', enabled: !busy && item.running, onInvoke: () => void wait(item.id) }),
        h(ConfirmAction, { authorityKey: `execution:${item.id}:SIGTERM`, label: 'Terminate', confirmLabel: 'Confirm SIGTERM', pendingLabel: 'Confirm SIGTERM', question: `Send SIGTERM to execution ${item.id}?`, enabled: !busy && item.running, onConfirm: () => terminate(item.id) }),
        h(ConfirmAction, { authorityKey: `execution:${item.id}:remove`, label: 'Remove record', confirmLabel: 'Confirm removal', pendingLabel: 'Confirm removal', question: `Remove execution record ${shortId(item.id)}?`, enabled: !busy && !item.running, onConfirm: () => remove(item.id) })))),
    h(Omitted, { count: view.omitted }),
    truncated ? h(Text, { label: 'The host execution catalogue was truncated at its safety limit.', color: 'warning', wrap: true }) : null);
}

const IMAGE_DETAIL_SCHEMA = Object.freeze([
  { key: 'property', title: 'Property', width: { chars: 20 } },
  { key: 'value', title: 'Value', width: 'fill' },
]);

export function Images({ api, resource, imageDetails }) {
  const localDetails = useMemo(() => new ImageDetailsSource(), []);
  const detailsSource = imageDetails ?? localDetails;
  const [reference, setReference] = useState('');
  const [detail, setDetail] = useState(null);
  const [confirm, setConfirm] = useState('');
  const [busy, setBusy] = useState('');
  const [error, setError] = useState(null);
  const [notice, setNotice] = useState('');
  const currentImages = useRef(new Set());
  currentImages.current = new Set((resource.data ?? []).map((item) => item.id));
  const [pull, setPull] = useState(null);
  const [inspection, setInspection] = useState({ id: '', state: 'idle', count: 0, error: null });
  const run = async (name, operation) => {
    setBusy(name); setError(null); setNotice('');
    try { await operation(); } catch (cause) { setError(cause); } finally { setBusy(''); }
  };
  const startPull = () => run('pull', async () => {
    if (typeof api.images.startPull !== 'function') { await api.images.pull(reference.trim()); await resource.reload(); return; }
    const started = await api.images.startPull(reference.trim());
    setPull({ job: started.job, reference: reference.trim(), revision: 0, state: 'starting', status: 'Starting pull…', layer: null, current: null, total: null, error: null });
  });
  useEffect(() => {
    if (!pull?.job || typeof api.watchImagePulls !== 'function' || ['complete', 'failed', 'cancelled'].includes(pull.state)) return undefined;
    let disposed = false; let stop = null;
    void api.watchImagePulls(async (change) => {
      if (disposed || change.job !== pull.job || change.revision <= pull.revision) return;
      const status = await api.images.pullStatus(pull.job);
      if (disposed) return;
      setPull(status);
      if (status.state === 'complete') { setNotice(`Pulled ${status.reference}.`); await resource.reload(); }
    }).then((dispose) => { if (disposed) void dispose(); else stop = dispose; }).catch((error) => {
      if (!disposed) setPull((current) => ({ ...current, state: 'failed', error: error.message ?? String(error) }));
    });
    return () => { disposed = true; if (stop) void stop(); };
  }, [api, pull?.job, pull?.revision, pull?.state, resource.reload]);
  const cancelPull = () => run('pull-cancel', async () => { await api.images.cancelPull(pull.job); setPull((current) => ({ ...current, state: 'cancelled', status: 'Pull cancelled.' })); });
  const inspect = async (item) => {
    setBusy(`inspect:${item.id}`);
    setDetail(null);
    setInspection({ id: item.id, state: 'loading', count: 0, error: null });
    try {
      const value = await api.images.inspect(item.reference || item.id);
      const count = await detailsSource.replace(value);
      setDetail(value);
      setInspection({ id: item.id, state: 'ready', count, error: null });
    } catch (cause) {
      setInspection({ id: item.id, state: 'error', count: 0, error: cause });
    } finally {
      setBusy('');
    }
  };
  const remove = (item) => run(`remove:${item.id}`, async () => {
    if (!currentImages.current.has(item.id)) throw new Error(`Image ${item.id} changed or disappeared; inspect and confirm again.`);
    await api.images.remove(item.id); setConfirm('');
    if (detail?.id === item.id) setDetail(null);
    await resource.reload();
  });
  const prune = () => run('prune', async () => {
    const result = await api.images.prune(); setConfirm('');
    setNotice(`Pruned ${result.deleted} image records and reclaimed ${bytes(result.space_reclaimed)}.`);
    await resource.reload();
  });
  const view = bounded(resource.data);
  return h(Page, { title: 'Images', subtitle: 'Images available to this workspace.' },
    h(Row, { gap: 1 }, h(Entry, { value: reference, placeholder: 'registry/image:tag', onChange: (event) => setReference(String(event.value ?? '')) }), h(Button, { label: pull?.state === 'failed' ? 'Retry pull' : busy === 'pull' ? 'Starting…' : 'Pull', enabled: !busy && reference.trim().length > 0 && (!pull || ['complete', 'failed', 'cancelled'].includes(pull.state)), onInvoke: startPull }), h(Button, { label: 'Refresh', enabled: !busy, onInvoke: resource.reload })),
    pull ? h(Card, { variant: pull.state === 'failed' ? 'outline' : 'filled' },
      h(CardHeader, { label: pull.reference, detail: pull.status ?? pull.state }),
      h(CardContent, { gap: 1 }, pull.total > 0 ? h(Meter, { fraction: Math.min(1, pull.current / pull.total), value: `${pull.current} / ${pull.total} bytes` }) : pull.state === 'pulling' || pull.state === 'starting' ? h(Progress, { busy: true }) : null,
        pull.layer ? h(Text, { label: `Layer ${pull.layer}`, color: 'text-dim' }) : null,
        pull.error ? h(Text, { label: pull.error, color: 'danger', wrap: true }) : null),
      h(CardActions, {}, !['complete', 'failed', 'cancelled'].includes(pull.state) ? h(Button, { label: 'Cancel pull', onInvoke: cancelPull }) : null)) : null,
    h(Row, { gap: 1, align: 'center' }, busy ? h(Spinner) : null, confirm === 'prune'
      ? h(React.Fragment, {}, h(Text, { label: 'Remove every unused image?', color: 'warning' }), h(Button, { label: 'Confirm prune', enabled: !busy, tone: 'danger', destructive: true, onInvoke: prune }), h(Button, { label: 'Cancel', enabled: !busy, onInvoke: () => setConfirm('') }))
      : h(Button, { label: 'Prune unused images', enabled: !busy, tone: 'danger', onInvoke: () => setConfirm('prune') })),
    h(ErrorText, { error: error ?? resource.error }), notice ? h(Text, { label: notice, color: 'positive' }) : null,
    h(InventoryEmpty, { resource: { loading: resource.loading, error: error ?? resource.error }, records: view.records, label: 'No images', detail: 'Enter an image reference above to pull one into this workspace.' }),
    ...view.records.map((item) => h(Card, { key: item.id, variant: detail?.id === item.id ? 'filled' : 'outline' }, h(CardHeader, { label: item.reference || item.repo_tags?.[0] || '<untagged>', detail: shortId(item.id) }),
      h(CardContent, {}, h(Text, { label: bytes(item.size), color: 'text-dim' }),
        inspection.id === item.id ? h(ResourceState, {
          state: inspection.state === 'ready' && inspection.count === 0 ? 'empty' : inspection.state,
          loadingLabel: 'Reading image details…',
          emptyLabel: 'No image details',
          emptyDetail: 'The host returned no inspectable fields.',
          error: inspection.error?.message ?? String(inspection.error ?? ''),
          retryLabel: 'Retry inspect',
          onRetry: () => inspect(item),
        }, h(StructuredDetail, { value: detail })) : null),
      h(CardActions, { gap: 1 }, h(Button, { label: 'Inspect', enabled: !busy, onInvoke: () => inspect(item) }), confirm === item.id
        ? h(React.Fragment, {}, h(Text, { label: `Remove immutable image ${item.id}?`, color: 'warning' }), h(Button, { label: 'Confirm remove', enabled: !busy, tone: 'danger', destructive: true, onInvoke: () => remove(item) }), h(Button, { label: 'Cancel', enabled: !busy, onInvoke: () => setConfirm('') }))
        : h(Button, { label: 'Remove', enabled: !busy, tone: 'danger', onInvoke: () => setConfirm(item.id) })))),
    h(Omitted, { count: view.omitted }));
}

export function Volumes({ api, resource, volumeDetails }) {
  const localDetails = useMemo(() => new VolumeDetailsSource(), []);
  const detailsSource = volumeDetails ?? localDetails;
  const [name, setName] = useState('');
  const [inspection, setInspection] = useState({ name: '', state: 'idle', count: 0, detail: null, error: null });
  const [creation, setCreation] = useState({ state: 'idle', name: '', error: null });
  const create = async () => {
    const requested = name.trim();
    if (!requested || creation.state === 'loading') return;
    setCreation({ state: 'loading', name: requested, error: null });
    try {
      await api.volumes.create(requested);
      await resource.reload();
      setName('');
      setCreation({ state: 'success', name: requested, error: null });
    } catch (cause) {
      setCreation({ state: 'error', name: requested, error: cause });
    }
  };
  const currentVolumes = useRef(new Map());
  currentVolumes.current = new Map((resource.data ?? []).map((volume) => [volume.name, volume.generation]));
  const remove = async (volume) => {
    if (currentVolumes.current.get(volume.name) !== volume.generation) {
      throw new Error(`Volume ${volume.name} changed generation; inspect and confirm again.`);
    }
    await api.volumes.remove(volume.name, volume.generation);
    if (inspection.name === volume.name) setInspection({ name: '', state: 'idle', count: 0, detail: null, error: null });
    await resource.reload();
  };
  const inspect = async (volume) => {
    setInspection({ name: volume.name, state: 'loading', count: 0, detail: null, error: null });
    try {
      const detail = await api.volumes.inspect(volume.name);
      const count = await detailsSource.replace(detail);
      setInspection({ name: volume.name, state: 'ready', count, detail, error: null });
    } catch (error) { setInspection({ name: volume.name, state: 'error', count: 0, detail: null, error }); }
  };
  const view = bounded(resource.data);
  return h(Page, { title: 'Volumes', subtitle: 'Bounded local volume inventory and safe, non-force lifecycle.' },
    h(Row, { gap: 1 }, h(Entry, { value: name, placeholder: 'Volume name', enabled: creation.state !== 'loading', onChange: (event) => { setName(String(event.value ?? '')); setCreation({ state: 'idle', name: '', error: null }); } }), h(Button, { label: creation.state === 'loading' ? 'Creating…' : creation.state === 'error' ? 'Retry create' : 'Create', enabled: creation.state !== 'loading' && name.trim().length > 0, onInvoke: () => { void create(); } }), h(Button, { label: 'Refresh', enabled: creation.state !== 'loading', onInvoke: resource.reload })),
    creation.state === 'loading' ? h(Row, { gap: 1, align: 'center' }, h(Spinner), h(Text, { label: `Creating volume ${creation.name}…` })) : null,
    creation.state === 'error' ? h(Text, { label: boundedMessage(creation.error), color: 'danger', wrap: true }) : null,
    creation.state === 'success' ? h(Text, { label: `Created volume ${creation.name}.`, color: 'positive', wrap: true }) : null,
    h(ErrorText, { error: resource.error }),
    h(InventoryEmpty, { resource, records: view.records, label: 'No volumes', detail: 'Create a named volume above when a workload needs durable storage.' }),
    ...view.records.map((volume) => h(Card, { key: `${volume.name}:${volume.generation}`, variant: inspection.name === volume.name ? 'filled' : 'outline' },
      h(CardHeader, { label: volume.name, detail: volume.driver }),
      h(CardActions, { gap: 1 }, h(Button, { label: inspection.name === volume.name && inspection.state === 'error' ? 'Retry inspect' : 'Inspect', onInvoke: () => inspect(volume) }), h(ConfirmAction, {
        authorityKey: `volume:${volume.name}:${volume.generation}:remove`,
        label: 'Remove', confirmLabel: 'Confirm remove', pendingLabel: 'Confirm remove', question: `Remove volume ${volume.name} generation ${volume.generation}?`,
        onConfirm: () => remove(volume),
      })),
      inspection.name === volume.name ? h(CardContent, {},
        inspection.state === 'loading'
          ? h(Row, { gap: 1, align: 'center' }, h(Spinner), h(Text, { label: 'Reading volume details…' }))
          : inspection.state === 'error'
            ? h(Text, { label: inspection.error?.message ?? String(inspection.error), color: 'danger', wrap: true })
            : inspection.count === 0
              ? h(EmptyState, { label: 'No volume details', detail: 'The host returned no inspectable fields.' })
              : h(StructuredDetail, { value: inspection.detail })) : null)),
    h(Omitted, { count: view.omitted }));
}

export function Networks({ api, resource, networkDetails }) {
  const localDetails = useMemo(() => new NetworkDetailsSource(), []);
  const detailsSource = networkDetails ?? localDetails;
  const [name, setName] = useState('');
  const [container, setContainer] = useState('');
  const [aliases, setAliases] = useState('');
  const [inspection, setInspection] = useState({ id: '', state: 'idle', count: 0, detail: null, error: null });
  const [error, setError] = useState(null);
  const [creation, setCreation] = useState({ state: 'idle', name: '', error: null });
  const [operation, setOperation] = useState({ state: 'idle', request: null, error: null });
  const [disconnectRequest, setDisconnectRequest] = useState(null);
  const endpointInput = useRef({ container: '', aliases: '' });
  endpointInput.current = { container: container.trim(), aliases };
  const currentNetworks = useRef(new Set());
  currentNetworks.current = new Set((resource.data ?? []).map(resourceReference));
  const current = (id) => { if (currentNetworks.current.has(id)) return true; setError(new Error(`Network ${id} changed or disappeared; inspect and confirm again.`)); return false; };
  const create = async () => {
    const requested = name.trim();
    if (!requested || creation.state === 'loading') return;
    setCreation({ state: 'loading', name: requested, error: null });
    try {
      await api.networks.create(requested);
      await resource.reload();
      setName('');
      setCreation({ state: 'success', name: requested, error: null });
    } catch (cause) {
      setCreation({ state: 'error', name: requested, error: cause });
    }
  };
  const remove = async (network) => { const id = resourceReference(network); if (!current(id)) return; await api.networks.remove(id); if (inspection.id === id) setInspection({ id: '', state: 'idle', count: 0, detail: null, error: null }); await resource.reload(); };
  const inspect = async (network) => {
    const id = resourceReference(network);
    setInspection({ id, state: 'loading', count: 0, detail: null, error: null });
    try {
      const detail = await api.networks.inspect(id);
      const count = await detailsSource.replace(detail);
      setInspection({ id, state: 'ready', count, detail, error: null });
    } catch (error) { setInspection({ id, state: 'error', count: 0, detail: null, error }); }
  };
  const request = (network, verb) => {
    const containerId = container.trim();
    if (!immutableContainerId(containerId)) throw new TypeError('Enter the complete 32- or 64-character lowercase hexadecimal container ID returned by inspection.');
    return { verb, network: resourceReference(network), container: containerId, aliases: verb === 'connect' ? endpointAliases(aliases) : [] };
  };
  const attach = async (next) => {
    if (next.container !== endpointInput.current.container
      || (next.verb === 'connect' && next.aliases.join(',') !== endpointAliases(endpointInput.current.aliases).join(','))) {
      throw new Error('Endpoint input changed; review and confirm the operation again.');
    }
    if (!current(next.network)) throw new Error(`Network ${next.network} changed or disappeared; inspect and confirm again.`);
    setOperation({ state: 'loading', request: next, error: null });
    try {
      if (next.verb === 'connect') await api.networks.connect(next.network, next.container, { aliases: next.aliases });
      else await api.networks.disconnect(next.network, next.container);
      await resource.reload();
      setOperation({ state: 'success', request: next, error: null });
      setDisconnectRequest(null);
    } catch (cause) {
      setOperation({ state: 'error', request: next, error: cause });
      throw cause;
    }
  };
  const begin = (network, verb) => {
    setError(null);
    try {
      const next = request(network, verb);
      if (verb === 'disconnect') setDisconnectRequest(next);
      else void attach(next).catch(() => {});
    } catch (cause) { setOperation({ state: 'error', request: null, error: cause }); }
  };
  const view = bounded(resource.data);
  return h(Page, { title: 'Networks', subtitle: 'Bounded network inventory; attachment changes are accepted only for stopped containers.' },
    h(Row, { gap: 1 }, h(Entry, { value: name, placeholder: 'Network name', enabled: creation.state !== 'loading', onChange: (event) => { setName(String(event.value ?? '')); setCreation({ state: 'idle', name: '', error: null }); } }), h(Button, { label: creation.state === 'loading' ? 'Creating…' : creation.state === 'error' ? 'Retry create' : 'Create', enabled: creation.state !== 'loading' && name.trim().length > 0, onInvoke: () => { void create(); } }), h(Button, { label: 'Refresh', enabled: creation.state !== 'loading', onInvoke: resource.reload })),
    creation.state === 'loading' ? h(Row, { gap: 1, align: 'center' }, h(Spinner), h(Text, { label: `Creating network ${creation.name}…` })) : null,
    creation.state === 'error' ? h(Text, { label: boundedMessage(creation.error), color: 'danger', wrap: true }) : null,
    creation.state === 'success' ? h(Text, { label: `Created network ${creation.name}.`, color: 'positive', wrap: true }) : null,
    h(Entry, { value: container, placeholder: 'Complete container ID', enabled: operation.state !== 'loading', onChange: (event) => { setContainer(String(event.value ?? '')); setOperation({ state: 'idle', request: null, error: null }); setDisconnectRequest(null); } }),
    h(Entry, { value: aliases, placeholder: 'Endpoint aliases (comma-separated, optional)', enabled: operation.state !== 'loading', onChange: (event) => { setAliases(String(event.value ?? '')); setOperation({ state: 'idle', request: null, error: null }); } }),
    operation.state === 'loading' ? h(Row, { gap: 1, align: 'center' }, h(Spinner), h(Text, { label: `${title(operation.request.verb)}ing immutable endpoint…` })) : null,
    operation.state === 'error' ? h(Row, { gap: 1, wrap: true }, h(Text, { label: boundedMessage(operation.error), color: 'danger', wrap: true }), operation.request ? h(Button, { label: `Retry ${operation.request.verb}`, onInvoke: () => { void attach(operation.request).catch(() => {}); } }) : null) : null,
    operation.state === 'success' ? h(Text, { label: `${operation.request.verb === 'connect' ? 'Connected' : 'Disconnected'} container ${operation.request.container} ${operation.request.verb === 'connect' ? 'to' : 'from'} network ${operation.request.network}${operation.request.aliases.length ? ` with ${operation.request.aliases.length} endpoint alias${operation.request.aliases.length === 1 ? '' : 'es'}` : ''}.`, color: 'positive', wrap: true }) : null,
    h(ErrorText, { error: error ?? resource.error }),
    h(InventoryEmpty, { resource, records: view.records, label: 'No networks', detail: 'Create a network above to connect workspace containers.' }),
    ...view.records.map((network) => h(Card, { key: resourceReference(network), variant: inspection.id === resourceReference(network) ? 'filled' : 'outline' },
      h(CardHeader, { label: network.name, detail: `${network.driver} · ${network.scope}` }),
      h(CardActions, { gap: 1 }, h(Button, { label: inspection.id === resourceReference(network) && inspection.state === 'error' ? 'Retry inspect' : 'Inspect', onInvoke: () => inspect(network) }), h(Button, { label: 'Connect', enabled: operation.state !== 'loading' && container.trim().length > 0, onInvoke: () => begin(network, 'connect') }), h(Button, {
        label: 'Disconnect', enabled: operation.state !== 'loading' && container.trim().length > 0, tone: 'danger', onInvoke: () => begin(network, 'disconnect'),
      }), h(ConfirmAction, {
        authorityKey: `network:${resourceReference(network)}:remove`,
        label: 'Remove', confirmLabel: 'Confirm remove', pendingLabel: 'Confirm remove', question: `Remove immutable network ${resourceReference(network)} (${network.name})?`,
        onConfirm: () => remove(network),
      })),
      disconnectRequest?.network === resourceReference(network) ? h(CardContent, {},
        h(Text, { label: `Disconnect immutable container ${disconnectRequest.container} from network ${disconnectRequest.network}?`, color: 'warning', wrap: true }),
        h(Row, { gap: 1 }, h(Button, { label: 'Confirm disconnect', enabled: operation.state !== 'loading', tone: 'danger', destructive: true, onInvoke: () => { void attach(disconnectRequest).catch(() => {}); } }), h(Button, { label: 'Cancel', enabled: operation.state !== 'loading', onInvoke: () => setDisconnectRequest(null) }))) : null,
      inspection.id === resourceReference(network) ? h(CardContent, {},
        inspection.state === 'loading'
          ? h(Row, { gap: 1, align: 'center' }, h(Spinner), h(Text, { label: 'Reading network details…' }))
          : inspection.state === 'error'
            ? h(Text, { label: inspection.error?.message ?? String(inspection.error), color: 'danger', wrap: true })
            : inspection.count === 0
              ? h(EmptyState, { label: 'No network details', detail: 'The host returned no inspectable fields.' })
              : h(StructuredDetail, { value: inspection.detail })) : null)),
    h(Omitted, { count: view.omitted }));
}

function Page({ title: label, subtitle, children }) { return h(Scroll, { grow: true, height: 'fill' }, h(Column, { pad: 4, gap: 2 }, h(Heading, { label, scale: 'title' }), h(Text, { label: subtitle, color: 'text-dim', wrap: true }), children)); }
function Toolbar({ loading, onRefresh }) { return h(Row, { gap: 1, align: 'center' }, loading ? h(Spinner) : null, h(Button, { label: 'Refresh', enabled: !loading, onInvoke: onRefresh })); }
function ErrorText({ error }) { return error ? h(Text, { label: boundedMessage(error), color: 'danger', wrap: true }) : null; }
function InventoryEmpty({ resource, records, label, detail }) {
  return !resource.loading && !resource.error && records.length === 0
    ? h(EmptyState, { label, detail })
    : null;
}
function Omitted({ count }) { return count > 0 ? h(Text, { label: `${count} more records omitted to keep this view bounded.`, color: 'text-dim' }) : null; }

function title(value) { return value.charAt(0).toUpperCase() + value.slice(1); }
function stateTone(state) { return state === 'running' ? 'positive' : state === 'paused' ? 'warning' : 'neutral'; }

function useResource(loader, initial) {
  const [data, setData] = useState(initial);
  const [loading, setLoading] = useState(initial === undefined);
  const [error, setError] = useState(null);
  const reload = useCallback(async () => {
    setLoading(true);
    try { setData(await loader()); setError(null); } catch (cause) { setError(cause); } finally { setLoading(false); }
  }, [loader]);
  const replace = useCallback((value) => { setData(value); setError(null); setLoading(false); }, []);
  useEffect(() => { if (initial === undefined) void reload(); }, [initial, reload]);
  return { data, loading, error, reload, replace };
}
