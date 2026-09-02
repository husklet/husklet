import React from 'react';
import {
  Badge, Button, Card, CardActions, CardContent, CardHeader, Column, Entry,
  EmptyState, Heading, KeyValueTable, List, ListItemButton, LogView, Row, Scroll, Separator, Spinner, Text,
  LOG_VIEW_CHARACTER_LIMIT,
} from '@husklet/react';
import { CONTAINER_DETAIL_SOURCE, ContainerDetailsSource, EXECUTION_DETAIL_SOURCE, ExecutionDetailsSource, IMAGE_DETAIL_SOURCE, ImageDetailsSource, NETWORK_DETAIL_SOURCE, NetworkDetailsSource, VOLUME_DETAIL_SOURCE, VolumeDetailsSource, LOG_LIMIT, bounded, bytes, logText, processRows, resourceReference, shortId } from './model.js';

const { createElement: h, useCallback, useEffect, useMemo, useState } = React;
const SECTIONS = ['overview', 'containers', 'processes', 'executions', 'images', 'volumes', 'networks'];

export function WorkspaceManager({ api, selections, containerDetails, executionDetails, imageDetails, networkDetails, volumeDetails, initial = {} }) {
  const [section, setSection] = useState('overview');
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
      ? h(Containers, { api, resource: containers, containerDetails })
      : section === 'processes'
        ? h(Processes, { api, resource: containers })
        : section === 'executions'
          ? h(Executions, { api, resource: executions, executionDetails, truncated: executionsTruncated })
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

export function Containers({ api, resource, containerDetails }) {
  const localDetails = useMemo(() => new ContainerDetailsSource(), []);
  const detailsSource = containerDetails ?? localDetails;
  const [selected, setSelected] = useState(null);
  const [busy, setBusy] = useState('');
  const [inspection, setInspection] = useState({ id: '', state: 'idle', count: 0, error: null });
  const act = async (verb, id, ...args) => {
    setBusy(`${verb}:${id}`);
    try { await api.containers[verb](id, ...args); await resource.reload(); } finally { setBusy(''); }
  };
  const inspect = async (item) => {
    setSelected(item.id);
    setInspection({ id: item.id, state: 'loading', count: 0, error: null });
    try {
      const detail = await api.containers.inspect(item.id);
      const count = await detailsSource.replace(detail);
      setInspection({ id: item.id, state: 'ready', count, error: null });
    } catch (cause) {
      setInspection({ id: item.id, state: 'error', count: 0, error: cause });
    }
  };
  const toggleDetails = (item) => {
    if (selected === item.id && inspection.state !== 'error') {
      setSelected(null);
      return;
    }
    void inspect(item);
  };
  const view = bounded(resource.data);
  return h(Page, { title: 'Containers', subtitle: 'Lifecycle, process inspection, logs, and execution.' },
    h(Toolbar, { loading: resource.loading, onRefresh: resource.reload }), h(ErrorText, { error: resource.error }),
    ...view.records.map((item) => h(Card, { key: item.id, variant: selected === item.id ? 'filled' : 'outline' },
      h(CardHeader, { label: item.name || shortId(item.id), detail: item.image }),
      h(CardContent, {}, h(Row, { gap: 2, align: 'center' }, h(Badge, { label: item.state, tone: stateTone(item.state) }), h(Text, { label: shortId(item.id), color: 'text-dim' }))),
      h(CardActions, { gap: 1 },
        h(Button, { label: selected === item.id && inspection.state === 'error' ? 'Retry details' : selected === item.id ? 'Hide details' : 'Details', onInvoke: () => toggleDetails(item) }),
        ...containerActions(item, busy, act)),
      selected === item.id ? h(ContainerDetail, { api, container: item, act, inspection }) : null)),
    h(Omitted, { count: view.omitted }));
}

function containerActions(item, busy, act) {
  const blocked = busy !== '';
  const running = item.state === 'running';
  return [
    h(Button, { key: 'start', label: running ? 'Restart' : 'Start', enabled: !blocked, onInvoke: () => act(running ? 'restart' : 'start', item.id) }),
    h(Button, { key: 'pause', label: item.state === 'paused' ? 'Resume' : 'Pause', enabled: !blocked && (running || item.state === 'paused'), onInvoke: () => act(item.state === 'paused' ? 'unpause' : 'pause', item.id) }),
    h(ConfirmAction, {
      key: 'stop', label: 'Stop', confirmLabel: 'Confirm stop',
      question: `Stop ${item.name || shortId(item.id)}?`, enabled: !blocked && running,
      onConfirm: () => act('stop', item.id),
    }),
  ];
}

function ContainerDetail({ api, container, act, inspection }) {
  const [command, setCommand] = useState('');
  const [logs, setLogs] = useState(null);
  const run = async () => {
    const argv = command.trim().split(/\s+/).filter(Boolean);
    if (argv.length) await api.containers.exec(container.id, { command: argv });
  };
  const readLogs = async () => setLogs(logText(await api.containers.logs(container.id, { stdout: true, stderr: true })).slice(-LOG_LIMIT * 160));
  return h(CardContent, { gap: 2 },
    inspection.state === 'loading'
      ? h(Row, { gap: 1, align: 'center' }, h(Spinner), h(Text, { label: 'Reading container details…' }))
      : inspection.state === 'error'
        ? h(Text, { label: inspection.error?.message ?? String(inspection.error), color: 'danger', wrap: true })
        : inspection.state === 'ready' && inspection.count === 0
          ? h(EmptyState, { label: 'No container details', detail: 'The host returned no inspectable fields.' })
          : inspection.state === 'ready'
            ? h(KeyValueTable, { source: CONTAINER_DETAIL_SOURCE, schema: IMAGE_DETAIL_SCHEMA, height: { minimum: { step: 10 }, maximum: { step: 28 } } })
            : null,
    h(Separator), h(Heading, { label: 'Quick actions', scale: 'caption' }),
    h(Row, { gap: 1 }, h(Entry, { value: command, placeholder: 'Command and arguments', onChange: (event) => setCommand(String(event.value ?? '')) }), h(Button, { label: 'Execute', enabled: command.trim().length > 0, onInvoke: run }), h(Button, { label: 'Load logs', onInvoke: readLogs }), h(ConfirmAction, {
      label: 'Kill', confirmLabel: 'Confirm kill', question: `Force-kill ${container.name || shortId(container.id)}?`,
      onConfirm: () => act('kill', container.id, 'SIGKILL'),
    })),
    logs === null ? null : h(Text, { label: logs || 'No log output.', wrap: true }));
}

function Processes({ api, resource }) {
  const [processes, setProcesses] = useState([]);
  const [error, setError] = useState(null);
  const load = useCallback(async () => {
    try {
      const groups = await Promise.all((resource.data ?? []).map(async (container) => ({ container, rows: await api.containers.processes(container.id) })));
      setProcesses(groups.flatMap(({ container, rows }) => processRows(rows, container.name || shortId(container.id))));
      setError(null);
    } catch (cause) { setError(cause); }
  }, [api, resource.data]);
  useEffect(() => { void load(); }, [load]);
  const view = bounded(processes);
  return h(Page, { title: 'Processes', subtitle: 'A bounded snapshot across all visible containers.' },
    h(Toolbar, { loading: resource.loading, onRefresh: load }), h(ErrorText, { error }),
    ...view.records.map((process, index) => {
      const pid = process.cells.PID ?? process.cells.Pid ?? process.cells.pid ?? '—';
      const command = process.cells.CMD ?? process.cells.Command ?? process.cells.COMMAND ?? process.values.at(-1) ?? 'Process';
      const detail = Object.entries(process.cells).filter(([key]) => !['PID', 'Pid', 'pid', 'CMD', 'Command', 'COMMAND'].includes(key)).map(([key, value]) => `${key} ${value}`).join(' · ');
      return h(Card, { key: `${process.container}:${pid}:${index}`, variant: 'outline' },
        h(CardHeader, { label: command, detail: process.container }),
        h(CardContent, {}, h(Row, { gap: 2 }, h(Badge, { label: `PID ${pid}` }), h(Text, { label: detail, color: 'text-dim' }))));
    }),
    h(Omitted, { count: view.omitted }));
}

export function Executions({ api, resource, executionDetails, truncated = false }) {
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
  const logs = async (id) => {
    setBusy(`logs:${id}`);
    try {
      const value = await api.containers.executionLogs(id, { stdout: true, stderr: true });
      const text = (bytes) => logText(bytes).slice(-LOG_VIEW_CHARACTER_LIMIT);
      setOutput((current) => ({ revision: (current?.revision ?? 0) + 1,
        stdout: text({ stdout: value.stdout, stderr: [] }), stderr: text({ stdout: [], stderr: value.stderr }), truncated: value.truncated }));
    } finally { setBusy(''); }
  };
  const wait = async (id) => {
    setBusy(`wait:${id}`);
    try { const detail = await api.containers.waitExecution(id, { timeoutMs: 5_000 }); await detailsSource.replace(detail); await resource.reload(); }
    finally { setBusy(''); }
  };
  const remove = async (id) => { await api.containers.removeExecution(id); setSelected(''); setOutput(null); await resource.reload(); };
  const view = bounded(resource.data);
  return h(Page, { title: 'Executions', subtitle: 'Bounded exec-session catalogue, status and captured output.' },
    h(Toolbar, { loading: resource.loading, onRefresh: resource.reload }), h(ErrorText, { error: resource.error }),
    ...view.records.map((item) => h(Card, { key: item.id, variant: selected === item.id ? 'filled' : 'outline' },
      h(CardHeader, { label: item.command?.join(' ') || shortId(item.id), detail: `container ${shortId(item.container_id)}` }),
      h(CardContent, {}, h(Badge, { label: item.running ? 'running' : `exited ${item.exit_code}`, tone: item.running ? 'positive' : 'neutral' }),
        selected !== item.id ? null : inspection.state === 'loading'
          ? h(Row, { gap: 1, align: 'center' }, h(Spinner), h(Text, { label: 'Reading execution details…' }))
          : inspection.state === 'error'
            ? h(Text, { label: inspection.error?.message ?? String(inspection.error), color: 'danger', wrap: true })
            : inspection.count === 0
              ? h(EmptyState, { label: 'No execution details', detail: 'The host returned no inspectable fields.' })
              : h(KeyValueTable, { source: EXECUTION_DETAIL_SOURCE, schema: IMAGE_DETAIL_SCHEMA, height: { minimum: { step: 10 }, maximum: { step: 28 } } }),
        selected === item.id && output ? h(Column, { gap: 1 },
          h(Heading, { label: 'Standard output', scale: 'caption' }), h(LogView, { key: `stdout-${output.revision}`, value: output.stdout || 'No stdout captured.', monospace: true }),
          h(Heading, { label: 'Standard error', scale: 'caption' }), h(LogView, { key: `stderr-${output.revision}`, value: output.stderr || 'No stderr captured.', monospace: true }),
          output.truncated ? h(Text, { label: 'Host output was truncated to its configured bound.', color: 'warning' }) : null) : null),
      h(CardActions, { gap: 1 },
        h(Button, { label: selected === item.id && inspection.state === 'error' ? 'Retry details' : selected === item.id ? 'Hide details' : 'Details', enabled: !busy, onInvoke: () => selected === item.id && inspection.state !== 'error' ? setSelected('') : void inspect(item.id) }),
        h(Button, { label: busy === `logs:${item.id}` ? 'Loading logs…' : 'Load output', enabled: !busy, onInvoke: () => void logs(item.id) }),
        h(Button, { label: busy === `wait:${item.id}` ? 'Waiting…' : 'Wait up to 5s', enabled: !busy && item.running, onInvoke: () => void wait(item.id) }),
        h(ConfirmAction, { label: 'Remove record', confirmLabel: 'Confirm removal', question: `Remove execution record ${shortId(item.id)}?`, enabled: !busy && !item.running, onConfirm: () => remove(item.id) })))),
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
  const [inspection, setInspection] = useState({ id: '', state: 'idle', count: 0, error: null });
  const run = async (name, operation) => {
    setBusy(name); setError(null); setNotice('');
    try { await operation(); } catch (cause) { setError(cause); } finally { setBusy(''); }
  };
  const pull = () => run('pull', async () => { await api.images.pull(reference.trim()); setReference(''); await resource.reload(); });
  const inspect = (item) => run(`inspect:${item.id}`, async () => {
    setDetail(null);
    setInspection({ id: item.id, state: 'loading', count: 0, error: null });
    try {
      const value = await api.images.inspect(item.reference || item.id);
      const count = await detailsSource.replace(value);
      setDetail(value);
      setInspection({ id: item.id, state: 'ready', count, error: null });
    } catch (cause) {
      setInspection({ id: item.id, state: 'error', count: 0, error: cause });
      throw cause;
    }
  });
  const remove = (item) => run(`remove:${item.id}`, async () => {
    await api.images.remove(item.reference || item.id); setConfirm('');
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
    h(Row, { gap: 1 }, h(Entry, { value: reference, placeholder: 'registry/image:tag', onChange: (event) => setReference(String(event.value ?? '')) }), h(Button, { label: busy === 'pull' ? 'Pulling…' : 'Pull', enabled: !busy && reference.trim().length > 0, onInvoke: pull }), h(Button, { label: 'Refresh', enabled: !busy, onInvoke: resource.reload })),
    h(Row, { gap: 1, align: 'center' }, busy ? h(Spinner) : null, confirm === 'prune'
      ? h(React.Fragment, {}, h(Text, { label: 'Remove every unused image?', color: 'warning' }), h(Button, { label: 'Confirm prune', enabled: !busy, tone: 'danger', destructive: true, onInvoke: prune }), h(Button, { label: 'Cancel', enabled: !busy, onInvoke: () => setConfirm('') }))
      : h(Button, { label: 'Prune unused images', enabled: !busy, tone: 'danger', onInvoke: () => setConfirm('prune') })),
    h(ErrorText, { error: error ?? resource.error }), notice ? h(Text, { label: notice, color: 'positive' }) : null,
    ...view.records.map((item) => h(Card, { key: item.id, variant: detail?.id === item.id ? 'filled' : 'outline' }, h(CardHeader, { label: item.reference || item.repo_tags?.[0] || '<untagged>', detail: shortId(item.id) }),
      h(CardContent, {}, h(Text, { label: bytes(item.size), color: 'text-dim' }),
        inspection.id === item.id && inspection.state === 'loading'
          ? h(Row, { gap: 1, align: 'center' }, h(Spinner), h(Text, { label: 'Reading image details…' }))
          : inspection.id === item.id && inspection.state === 'error'
            ? h(Text, { label: inspection.error?.message ?? String(inspection.error), color: 'danger', wrap: true })
            : inspection.id === item.id && inspection.state === 'ready' && inspection.count === 0
              ? h(EmptyState, { label: 'No image details', detail: 'The host returned no inspectable fields.' })
              : detail?.id === item.id
                ? h(KeyValueTable, { source: IMAGE_DETAIL_SOURCE, schema: IMAGE_DETAIL_SCHEMA, height: { minimum: { step: 12 }, maximum: { step: 40 } } })
                : null),
      h(CardActions, { gap: 1 }, h(Button, { label: inspection.id === item.id && inspection.state === 'error' ? 'Retry inspect' : 'Inspect', enabled: !busy, onInvoke: () => inspect(item) }), confirm === item.id
        ? h(React.Fragment, {}, h(Text, { label: 'Remove this local image?', color: 'warning' }), h(Button, { label: 'Confirm remove', enabled: !busy, tone: 'danger', destructive: true, onInvoke: () => remove(item) }), h(Button, { label: 'Cancel', enabled: !busy, onInvoke: () => setConfirm('') }))
        : h(Button, { label: 'Remove', enabled: !busy, tone: 'danger', onInvoke: () => setConfirm(item.id) })))),
    h(Omitted, { count: view.omitted }));
}

export function Volumes({ api, resource, volumeDetails }) {
  const localDetails = useMemo(() => new VolumeDetailsSource(), []);
  const detailsSource = volumeDetails ?? localDetails;
  const [name, setName] = useState('');
  const [inspection, setInspection] = useState({ name: '', state: 'idle', count: 0, error: null });
  const create = async () => { await api.volumes.create(name.trim()); setName(''); await resource.reload(); };
  const remove = async (volume) => { await api.volumes.remove(volume.name); if (inspection.name === volume.name) setInspection({ name: '', state: 'idle', count: 0, error: null }); await resource.reload(); };
  const inspect = async (volume) => {
    setInspection({ name: volume.name, state: 'loading', count: 0, error: null });
    try {
      const count = await detailsSource.replace(await api.volumes.inspect(volume.name));
      setInspection({ name: volume.name, state: 'ready', count, error: null });
    } catch (error) { setInspection({ name: volume.name, state: 'error', count: 0, error }); }
  };
  const view = bounded(resource.data);
  return h(Page, { title: 'Volumes', subtitle: 'Bounded local volume inventory and safe, non-force lifecycle.' },
    h(Row, { gap: 1 }, h(Entry, { value: name, placeholder: 'Volume name', onChange: (event) => setName(String(event.value ?? '')) }), h(Button, { label: 'Create', enabled: name.trim().length > 0, onInvoke: create }), h(Button, { label: 'Refresh', onInvoke: resource.reload })),
    h(ErrorText, { error: resource.error }),
    ...view.records.map((volume) => h(Card, { key: volume.name, variant: inspection.name === volume.name ? 'filled' : 'outline' },
      h(CardHeader, { label: volume.name, detail: volume.driver }),
      h(CardActions, { gap: 1 }, h(Button, { label: inspection.name === volume.name && inspection.state === 'error' ? 'Retry inspect' : 'Inspect', onInvoke: () => inspect(volume) }), h(ConfirmAction, {
        label: 'Remove', confirmLabel: 'Confirm remove', question: `Remove volume ${volume.name}?`,
        onConfirm: () => remove(volume),
      })),
      inspection.name === volume.name ? h(CardContent, {},
        inspection.state === 'loading'
          ? h(Row, { gap: 1, align: 'center' }, h(Spinner), h(Text, { label: 'Reading volume details…' }))
          : inspection.state === 'error'
            ? h(Text, { label: inspection.error?.message ?? String(inspection.error), color: 'danger', wrap: true })
            : inspection.count === 0
              ? h(EmptyState, { label: 'No volume details', detail: 'The host returned no inspectable fields.' })
              : h(KeyValueTable, { source: VOLUME_DETAIL_SOURCE, schema: IMAGE_DETAIL_SCHEMA, height: { minimum: { step: 6 }, maximum: { step: 10 } } })) : null)),
    h(Omitted, { count: view.omitted }));
}

export function Networks({ api, resource, networkDetails }) {
  const localDetails = useMemo(() => new NetworkDetailsSource(), []);
  const detailsSource = networkDetails ?? localDetails;
  const [name, setName] = useState('');
  const [container, setContainer] = useState('');
  const [inspection, setInspection] = useState({ id: '', state: 'idle', count: 0, error: null });
  const create = async () => { await api.networks.create(name.trim()); setName(''); await resource.reload(); };
  const remove = async (network) => { await api.networks.remove(resourceReference(network)); if (inspection.id === resourceReference(network)) setInspection({ id: '', state: 'idle', count: 0, error: null }); await resource.reload(); };
  const inspect = async (network) => {
    const id = resourceReference(network);
    setInspection({ id, state: 'loading', count: 0, error: null });
    try {
      const count = await detailsSource.replace(await api.networks.inspect(id));
      setInspection({ id, state: 'ready', count, error: null });
    } catch (error) { setInspection({ id, state: 'error', count: 0, error }); }
  };
  const attach = async (network, verb) => { await api.networks[verb](resourceReference(network), container.trim()); await resource.reload(); };
  const view = bounded(resource.data);
  return h(Page, { title: 'Networks', subtitle: 'Bounded network inventory; attachment changes are accepted only for stopped containers.' },
    h(Row, { gap: 1 }, h(Entry, { value: name, placeholder: 'Network name', onChange: (event) => setName(String(event.value ?? '')) }), h(Button, { label: 'Create', enabled: name.trim().length > 0, onInvoke: create }), h(Button, { label: 'Refresh', onInvoke: resource.reload })),
    h(Entry, { value: container, placeholder: 'Container ID for connect/disconnect', onChange: (event) => setContainer(String(event.value ?? '')) }),
    h(ErrorText, { error: resource.error }),
    ...view.records.map((network) => h(Card, { key: resourceReference(network), variant: inspection.id === resourceReference(network) ? 'filled' : 'outline' },
      h(CardHeader, { label: network.name, detail: `${network.driver} · ${network.scope}` }),
      h(CardActions, { gap: 1 }, h(Button, { label: inspection.id === resourceReference(network) && inspection.state === 'error' ? 'Retry inspect' : 'Inspect', onInvoke: () => inspect(network) }), h(Button, { label: 'Connect', enabled: container.trim().length > 0, onInvoke: () => attach(network, 'connect') }), h(ConfirmAction, {
        label: 'Disconnect', confirmLabel: 'Confirm disconnect',
        question: `Disconnect ${container.trim() || 'container'} from ${network.name}?`,
        enabled: container.trim().length > 0, onConfirm: () => attach(network, 'disconnect'),
      }), h(ConfirmAction, {
        label: 'Remove', confirmLabel: 'Confirm remove', question: `Remove network ${network.name}?`,
        onConfirm: () => remove(network),
      })),
      inspection.id === resourceReference(network) ? h(CardContent, {},
        inspection.state === 'loading'
          ? h(Row, { gap: 1, align: 'center' }, h(Spinner), h(Text, { label: 'Reading network details…' }))
          : inspection.state === 'error'
            ? h(Text, { label: inspection.error?.message ?? String(inspection.error), color: 'danger', wrap: true })
            : inspection.count === 0
              ? h(EmptyState, { label: 'No network details', detail: 'The host returned no inspectable fields.' })
              : h(KeyValueTable, { source: NETWORK_DETAIL_SOURCE, schema: IMAGE_DETAIL_SCHEMA, height: { minimum: { step: 8 }, maximum: { step: 16 } } })) : null)),
    h(Omitted, { count: view.omitted }));
}

function Page({ title: label, subtitle, children }) { return h(Scroll, { grow: true, height: 'fill' }, h(Column, { pad: 4, gap: 2 }, h(Heading, { label, scale: 'title' }), h(Text, { label: subtitle, color: 'text-dim', wrap: true }), children)); }
function Toolbar({ loading, onRefresh }) { return h(Row, { gap: 1, align: 'center' }, loading ? h(Spinner) : null, h(Button, { label: 'Refresh', enabled: !loading, onInvoke: onRefresh })); }
function ErrorText({ error }) { return error ? h(Text, { label: error.message ?? String(error), color: 'danger', wrap: true }) : null; }
function Omitted({ count }) { return count > 0 ? h(Text, { label: `${count} more records omitted to keep this view bounded.`, color: 'text-dim' }) : null; }

// A destructive operation is always two distinct interactions. The first
// only reveals this prompt; only the final button carries destructive
// metadata and can call the host API.
function ConfirmAction({ label, confirmLabel, question, enabled = true, onConfirm }) {
  const [confirming, setConfirming] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState(null);
  const confirm = async () => {
    setBusy(true); setError(null);
    try { await onConfirm(); setConfirming(false); } catch (cause) { setError(cause); } finally { setBusy(false); }
  };
  if (!confirming) return h(Button, {
    label, enabled: enabled && !busy, tone: 'danger', onInvoke: () => { setError(null); setConfirming(true); },
  });
  return h(Column, { gap: 1 },
    h(Text, { label: question, color: 'warning', wrap: true }),
    h(Row, { gap: 1, align: 'center' }, busy ? h(Spinner) : null,
      h(Button, { label: confirmLabel, enabled: !busy, tone: 'danger', destructive: true, onInvoke: confirm }),
      h(Button, { label: 'Cancel', enabled: !busy, onInvoke: () => { setError(null); setConfirming(false); } })),
    h(ErrorText, { error }));
}
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
