import React from 'react';
import {
  Badge, Button, Card, CardActions, CardContent, CardHeader, Column, Entry,
  Heading, List, ListItemButton, Row, Scroll, Separator, Spinner, Text,
} from '@husklet/react';
import { LOG_LIMIT, bounded, bytes, logText, processRows, resourceReference, shortId } from './model.js';

const { createElement: h, useCallback, useEffect, useState } = React;
const SECTIONS = ['overview', 'containers', 'processes', 'images', 'volumes', 'networks'];

export function WorkspaceManager({ api, selections, initial = {} }) {
  const [section, setSection] = useState('overview');
  const containers = useResource(api.containers.list, initial.containers);
  const images = useResource(api.images.list, initial.images);
  const volumes = useResource(api.volumes.list, initial.volumes);
  const networks = useResource(api.networks.list, initial.networks);
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
      ? h(Containers, { api, resource: containers })
      : section === 'processes'
        ? h(Processes, { api, resource: containers })
        : section === 'images'
          ? h(Images, { api, resource: images })
          : section === 'volumes'
            ? h(Volumes, { api, resource: volumes })
            : h(Networks, { api, resource: networks });
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

function Containers({ api, resource }) {
  const [selected, setSelected] = useState(null);
  const [busy, setBusy] = useState('');
  const act = async (verb, id, ...args) => {
    setBusy(`${verb}:${id}`);
    try { await api.containers[verb](id, ...args); await resource.reload(); } finally { setBusy(''); }
  };
  const view = bounded(resource.data);
  return h(Page, { title: 'Containers', subtitle: 'Lifecycle, process inspection, logs, and execution.' },
    h(Toolbar, { loading: resource.loading, onRefresh: resource.reload }), h(ErrorText, { error: resource.error }),
    ...view.records.map((item) => h(Card, { key: item.id, variant: selected === item.id ? 'filled' : 'outline' },
      h(CardHeader, { label: item.name || shortId(item.id), detail: item.image }),
      h(CardContent, {}, h(Row, { gap: 2, align: 'center' }, h(Badge, { label: item.state, tone: stateTone(item.state) }), h(Text, { label: shortId(item.id), color: 'text-dim' }))),
      h(CardActions, { gap: 1 },
        h(Button, { label: selected === item.id ? 'Hide details' : 'Details', onInvoke: () => setSelected(selected === item.id ? null : item.id) }),
        ...containerActions(item, busy, act)),
      selected === item.id ? h(ContainerDetail, { api, container: item, act }) : null)),
    h(Omitted, { count: view.omitted }));
}

function containerActions(item, busy, act) {
  const blocked = busy !== '';
  const running = item.state === 'running';
  return [
    h(Button, { key: 'start', label: running ? 'Restart' : 'Start', enabled: !blocked, onInvoke: () => act(running ? 'restart' : 'start', item.id) }),
    h(Button, { key: 'pause', label: item.state === 'paused' ? 'Resume' : 'Pause', enabled: !blocked && (running || item.state === 'paused'), onInvoke: () => act(item.state === 'paused' ? 'unpause' : 'pause', item.id) }),
    h(Button, { key: 'stop', label: 'Stop', enabled: !blocked && running, tone: 'danger', onInvoke: () => act('stop', item.id) }),
  ];
}

function ContainerDetail({ api, container, act }) {
  const [command, setCommand] = useState('');
  const [logs, setLogs] = useState(null);
  const run = async () => {
    const argv = command.trim().split(/\s+/).filter(Boolean);
    if (argv.length) await api.containers.exec(container.id, { command: argv });
  };
  const readLogs = async () => setLogs(logText(await api.containers.logs(container.id, { stdout: true, stderr: true })).slice(-LOG_LIMIT * 160));
  return h(CardContent, { gap: 2 },
    h(Separator), h(Heading, { label: 'Quick actions', scale: 'caption' }),
    h(Row, { gap: 1 }, h(Entry, { value: command, placeholder: 'Command and arguments', onChange: (event) => setCommand(String(event.value ?? '')) }), h(Button, { label: 'Execute', enabled: command.trim().length > 0, onInvoke: run }), h(Button, { label: 'Load logs', onInvoke: readLogs }), h(Button, { label: 'Kill', tone: 'danger', onInvoke: () => act('kill', container.id, 'SIGKILL') })),
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

function Images({ api, resource }) {
  const [reference, setReference] = useState('');
  const pull = async () => { await api.images.pull(reference.trim()); setReference(''); await resource.reload(); };
  const view = bounded(resource.data);
  return h(Page, { title: 'Images', subtitle: 'Images available to this workspace.' },
    h(Row, { gap: 1 }, h(Entry, { value: reference, placeholder: 'registry/image:tag', onChange: (event) => setReference(String(event.value ?? '')) }), h(Button, { label: 'Pull', enabled: reference.trim().length > 0, onInvoke: pull }), h(Button, { label: 'Refresh', onInvoke: resource.reload })),
    h(ErrorText, { error: resource.error }),
    ...view.records.map((item) => h(Card, { key: item.id, variant: 'outline' }, h(CardHeader, { label: item.reference || item.repo_tags?.[0] || '<untagged>', detail: shortId(item.id) }), h(CardContent, {}, h(Text, { label: bytes(item.size), color: 'text-dim' })))),
    h(Omitted, { count: view.omitted }));
}

export function Volumes({ api, resource }) {
  const [name, setName] = useState('');
  const [detail, setDetail] = useState(null);
  const create = async () => { await api.volumes.create(name.trim()); setName(''); await resource.reload(); };
  const remove = async (volume) => { await api.volumes.remove(volume.name); if (detail?.name === volume.name) setDetail(null); await resource.reload(); };
  const inspect = async (volume) => setDetail(await api.volumes.inspect(volume.name));
  const view = bounded(resource.data);
  return h(Page, { title: 'Volumes', subtitle: 'Bounded local volume inventory and safe, non-force lifecycle.' },
    h(Row, { gap: 1 }, h(Entry, { value: name, placeholder: 'Volume name', onChange: (event) => setName(String(event.value ?? '')) }), h(Button, { label: 'Create', enabled: name.trim().length > 0, onInvoke: create }), h(Button, { label: 'Refresh', onInvoke: resource.reload })),
    h(ErrorText, { error: resource.error }),
    ...view.records.map((volume) => h(Card, { key: volume.name, variant: detail?.name === volume.name ? 'filled' : 'outline' },
      h(CardHeader, { label: volume.name, detail: volume.driver }),
      h(CardActions, { gap: 1 }, h(Button, { label: 'Inspect', onInvoke: () => inspect(volume) }), h(Button, { label: 'Remove', tone: 'danger', onInvoke: () => remove(volume) })),
      detail?.name === volume.name ? h(CardContent, {}, h(Text, { label: `Driver ${detail.driver}`, color: 'text-dim' })) : null)),
    h(Omitted, { count: view.omitted }));
}

export function Networks({ api, resource }) {
  const [name, setName] = useState('');
  const [container, setContainer] = useState('');
  const [detail, setDetail] = useState(null);
  const create = async () => { await api.networks.create(name.trim()); setName(''); await resource.reload(); };
  const remove = async (network) => { await api.networks.remove(resourceReference(network)); if (detail?.id === network.id) setDetail(null); await resource.reload(); };
  const inspect = async (network) => setDetail(await api.networks.inspect(resourceReference(network)));
  const attach = async (network, verb) => { await api.networks[verb](resourceReference(network), container.trim()); await resource.reload(); };
  const view = bounded(resource.data);
  return h(Page, { title: 'Networks', subtitle: 'Bounded network inventory; attachment changes are accepted only for stopped containers.' },
    h(Row, { gap: 1 }, h(Entry, { value: name, placeholder: 'Network name', onChange: (event) => setName(String(event.value ?? '')) }), h(Button, { label: 'Create', enabled: name.trim().length > 0, onInvoke: create }), h(Button, { label: 'Refresh', onInvoke: resource.reload })),
    h(Entry, { value: container, placeholder: 'Container ID for connect/disconnect', onChange: (event) => setContainer(String(event.value ?? '')) }),
    h(ErrorText, { error: resource.error }),
    ...view.records.map((network) => h(Card, { key: resourceReference(network), variant: detail?.id === network.id ? 'filled' : 'outline' },
      h(CardHeader, { label: network.name, detail: `${network.driver} · ${network.scope}` }),
      h(CardActions, { gap: 1 }, h(Button, { label: 'Inspect', onInvoke: () => inspect(network) }), h(Button, { label: 'Connect', enabled: container.trim().length > 0, onInvoke: () => attach(network, 'connect') }), h(Button, { label: 'Disconnect', enabled: container.trim().length > 0, onInvoke: () => attach(network, 'disconnect') }), h(Button, { label: 'Remove', tone: 'danger', onInvoke: () => remove(network) })),
      detail?.id === network.id ? h(CardContent, {}, h(Text, { label: `${detail.id} · ${detail.driver} · ${detail.scope}`, color: 'text-dim', wrap: true })) : null)),
    h(Omitted, { count: view.omitted }));
}

function Page({ title: label, subtitle, children }) { return h(Scroll, { grow: true, height: 'fill' }, h(Column, { pad: 4, gap: 2 }, h(Heading, { label, scale: 'title' }), h(Text, { label: subtitle, color: 'text-dim', wrap: true }), children)); }
function Toolbar({ loading, onRefresh }) { return h(Row, { gap: 1, align: 'center' }, loading ? h(Spinner) : null, h(Button, { label: 'Refresh', enabled: !loading, onInvoke: onRefresh })); }
function ErrorText({ error }) { return error ? h(Text, { label: error.message ?? String(error), color: 'danger', wrap: true }) : null; }
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
  useEffect(() => { if (initial === undefined) void reload(); }, [initial, reload]);
  return { data, loading, error, reload };
}
