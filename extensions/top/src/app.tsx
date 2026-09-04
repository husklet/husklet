// @ts-nocheck -- operational pages are migrated after the strict overview shell.
import React from 'react';
import {
  Badge, Button, Card, CardActions, CardContent, CardHeader, Column, Entry,
  ConfirmAction, EmptyState, Heading, KeyValueTable, List, ListItemButton, LogView, Meter, ObjectInspector, ResourceState, Row, Scroll, Separator, Spinner, Text,
  LOG_VIEW_CHARACTER_LIMIT,
  type ContainerSummary, type ExecutionSummary, type HostEvent, type ImageSummary, type NetworkSummary,
  type TabSummary, type VolumeSummary, type WorkspaceApi,
} from '@husklet/react';
import { ContainerDetailsSource, EXECUTION_DETAIL_SOURCE, ExecutionDetailsSource, ImageDetailsSource, NetworkDetailsSource, VolumeDetailsSource, LOG_LIMIT, bounded, boundedMessage, bytes, containerNameError, endpointAliases, immutableContainerId, logText, processRows, resourceReference, shortId } from './model.js';
import { Navigation, Overview, SECTIONS, type Resource, type Section } from './overview.js';

export { Overview, SECTIONS } from './overview.js';

const { useCallback, useEffect, useMemo, useRef, useState } = React;
type Selections = { subscribe(listener: (event: HostEvent) => void): (() => void) | undefined };
type TopProps = {
  api: WorkspaceApi;
  selections?: Selections;
  containerDetails?: ContainerDetailsSource;
  executionDetails?: ExecutionDetailsSource;
  imageDetails?: ImageDetailsSource;
  networkDetails?: NetworkDetailsSource;
  volumeDetails?: VolumeDetailsSource;
  initial?: Partial<{
    containers: ContainerSummary[]; executions: ExecutionSummary[]; images: ImageSummary[];
    volumes: VolumeSummary[]; networks: NetworkSummary[]; terminals: TabSummary[];
  }>;
};
const INSPECTOR_BOUNDS = Object.freeze({ maxDepth: 8, maxNodes: 128, maxStringLength: 256 });
const PROCESS_SAMPLING_CONCURRENCY = 8;

function StructuredDetail({ value }: { value: unknown }) {
  return (
    <ObjectInspector
      value={value}
      {...INSPECTOR_BOUNDS}
      height={{ minimum: { step: 10 }, maximum: { step: 32 } }} />
  );
}

export function Top({ api, selections, containerDetails, executionDetails, imageDetails, networkDetails, volumeDetails, initial = {} }: TopProps) {
  const [section, setSection] = useState<Section>('overview');
  const [requestedExecution, setRequestedExecution] = useState('');
  const containers = useResource(api.containers.list, initial.containers);
  const images = useResource(api.images.list, initial.images);
  const volumes = useResource(api.volumes.list, initial.volumes);
  const networks = useResource(api.networks.list, initial.networks);
  const terminals = useResource(api.terminal?.tabs ?? (async () => []), initial.terminals);
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
    if ('pane_provider' in event && SECTIONS.includes(event.pane_provider as Section)) setSection(event.pane_provider as Section);
    if (event?.snapshot === 'containers') void containers.reload();
    if (event?.snapshot === 'images') void images.reload();
    if (event?.snapshot === 'volumes') void volumes.reload();
    if (event?.snapshot === 'networks') void networks.reload();
    if (event?.snapshot === 'terminal') void terminals.reload();
  }), [selections, containers.reload, images.reload, volumes.reload, networks.reload, terminals.reload]);
  useEffect(() => {
    if (typeof api.subscribe !== 'function') return undefined;
    void api.subscribe('containers');
    void api.subscribe('images');
    void api.subscribe('volumes');
    void api.subscribe('networks');
    void api.subscribe('terminal');
    return () => {
      if (typeof api.unsubscribe === 'function') {
        void api.unsubscribe('containers');
        void api.unsubscribe('images');
        void api.unsubscribe('volumes');
        void api.unsubscribe('networks');
        void api.unsubscribe('terminal');
      }
    };
  }, [api]);
  const body = section === 'overview'
    ? <Overview
    containers={containers}
    images={images}
    volumes={volumes}
    networks={networks}
    terminals={terminals}
    onOpen={setSection} />
    : section === 'containers'
      ? <Containers
    api={api}
    resource={containers}
    containerDetails={containerDetails}
    onOpenExecution={async (id) => {
          setRequestedExecution(id);
          await executions.reload();
          setSection('executions');
        }} />
      : section === 'processes'
        ? <Processes api={api} resource={containers} />
        : section === 'executions'
          ? <Executions
    api={api}
    resource={executions}
    executionDetails={executionDetails}
    truncated={executionsTruncated}
    requestedExecution={requestedExecution} />
        : section === 'images'
          ? <Images api={api} resource={images} imageDetails={imageDetails} />
          : section === 'volumes'
            ? <Volumes api={api} resource={volumes} volumeDetails={volumeDetails} />
            : section === 'networks'
              ? <Networks api={api} resource={networks} networkDetails={networkDetails} />
              : <Terminals api={api} resource={terminals} />;
  return (
    <Row grow={true} gap={0}>
      <Navigation section={section} onSelect={setSection} />
      <Separator orientation={'vertical'} />
      {body}
    </Row>
  );
}

function containerCreateOptions(draft) {
  const bytes = (value) => new TextEncoder().encode(value).byteLength;
  const hostname = draft.hostname;
  if (hostname && (bytes(hostname) > 253 || !/^[A-Za-z0-9][A-Za-z0-9_.-]*$/.test(hostname))) {
    throw new Error('Hostname must start with an ASCII letter or digit, contain only ASCII letters, digits, dots, underscores or hyphens, and be at most 253 bytes.');
  }
  const user = draft.user;
  if (user && (bytes(user) > 256 || user.includes('\0'))) {
    throw new Error('Run as user must be a nonempty, NUL-free value of at most 256 bytes.');
  }
  const labelsText = draft.labels.trim();
  let labels;
  if (labelsText) {
    try { labels = JSON.parse(labelsText); } catch { throw new Error('Labels must be valid JSON pairs, such as [["role","worker"]].'); }
    if (!Array.isArray(labels) || labels.length > 128
      || labels.some((pair) => !Array.isArray(pair) || pair.length !== 2
        || pair.some((value) => typeof value !== 'string')
        || pair[0].length === 0 || pair[0].includes('\0') || bytes(pair[0]) > 256
        || pair[1].includes('\0') || bytes(pair[1]) > 4_096)
      || new Set(labels.map(([name]) => name)).size !== labels.length) {
      throw new Error('Labels must contain at most 128 unique [name, value] pairs; names are nonempty and at most 256 bytes, values at most 4096 bytes, and both are NUL-free.');
    }
  }
  const network = draft.network;
  if (network && (bytes(network) > 255 || !/^[A-Za-z0-9][A-Za-z0-9_.-]*$/.test(network))) {
    throw new Error('Initial network must start with an ASCII letter or digit, contain only ASCII letters, digits, dots, underscores or hyphens, and be at most 255 bytes.');
  }
  const entrypointText = draft.entrypoint.trim();
  let entrypoint;
  if (entrypointText) {
    try { entrypoint = JSON.parse(entrypointText); } catch { throw new Error('Entrypoint must be valid JSON, such as ["/bin/sh","-lc"].'); }
    if (!Array.isArray(entrypoint) || entrypoint.length === 0 || entrypoint.length > 64 || entrypoint[0] === ''
      || entrypoint.some((argument) => typeof argument !== 'string' || argument.includes('\0') || bytes(argument) > 4_096)
      || entrypoint.reduce((total, argument) => total + bytes(argument), 0) > 32_768) {
      throw new Error('Entrypoint must contain 1 to 64 NUL-free string arguments, each at most 4096 bytes and 32768 bytes in total.');
    }
  }
  const commandText = draft.command.trim();
  let command;
  if (commandText) {
    try { command = JSON.parse(commandText); } catch { throw new Error('Command must be valid JSON, such as ["sh","-lc","printf ready"].'); }
    if (!Array.isArray(command) || command.length > 64 || (command.length > 0 && command[0] === '')
      || command.some((argument) => typeof argument !== 'string' || argument.includes('\0') || bytes(argument) > 4_096)
      || command.reduce((total, argument) => total + bytes(argument), 0) > 32_768) {
      throw new Error('Command must contain at most 64 NUL-free string arguments, each at most 4096 bytes and 32768 bytes in total.');
    }
  }
  if ([...(entrypoint ?? []), ...(command ?? [])].reduce((total, argument) => total + bytes(argument), 0) > 32_768) {
    throw new Error('Entrypoint and command together must contain at most 32768 bytes.');
  }
  const environmentText = draft.environment.trim();
  let environment;
  if (environmentText) {
    try { environment = JSON.parse(environmentText); } catch { throw new Error('Environment must be valid JSON pairs, such as [["MODE","test"]].'); }
    if (!Array.isArray(environment) || environment.length > 256
      || environment.some((pair) => !Array.isArray(pair) || pair.length !== 2
        || pair.some((value) => typeof value !== 'string')
        || pair[0].length === 0 || pair[0].includes('=') || pair[0].includes('\0') || bytes(pair[0]) > 256
        || pair[1].includes('\0') || bytes(pair[1]) > 8_192)
      || new Set(environment.map(([name]) => name)).size !== environment.length) {
      throw new Error('Environment must contain at most 256 unique [name, value] pairs with bounded NUL-free strings.');
    }
  }
  const workingDirectory = draft.workingDirectory.trim();
  if (workingDirectory && (!workingDirectory.startsWith('/') || bytes(workingDirectory) > 4_096
    || workingDirectory.includes('\0') || workingDirectory.split('/').some((part) => part === '.' || part === '..'))) {
    throw new Error('Working directory must be an absolute, NUL-free path without dot segments and at most 4096 bytes.');
  }
  const memoryMb = optionalDecimalLimit(draft.memoryMb, 'Memory limit', 1_048_576);
  const cpus = optionalDecimalLimit(draft.cpus, 'CPU limit', 256);
  const pidsLimit = optionalDecimalLimit(draft.pidsLimit, 'PID limit', 1_000_000);
  const mountsText = draft.mounts.trim();
  let mounts;
  if (mountsText) {
    try { mounts = JSON.parse(mountsText); } catch { throw new Error('Mounts must be valid JSON, such as [{"volume":"cache","target":"/cache","read_only":true}].'); }
    const allowed = new Set(['volume', 'target', 'read_only']);
    if (!Array.isArray(mounts) || mounts.length > 64
      || mounts.some((mount) => !mount || typeof mount !== 'object' || Array.isArray(mount)
        || Object.keys(mount).some((key) => !allowed.has(key))
        || typeof mount.volume !== 'string' || !/^[A-Za-z0-9][A-Za-z0-9_.-]{0,254}$/.test(mount.volume)
        || typeof mount.target !== 'string' || !mount.target.startsWith('/') || bytes(mount.target) > 4_096
        || mount.target.includes('\0') || mount.target.split('/').some((part) => part === '.' || part === '..')
        || (mount.read_only !== undefined && typeof mount.read_only !== 'boolean'))
      || new Set(mounts.map((mount) => mount.target)).size !== mounts.length) {
      throw new Error('Mounts must contain at most 64 named volumes with unique absolute targets and optional boolean read_only. Host bind mounts are not accepted.');
    }
    mounts = mounts.map(({ volume, target, read_only = false }) => ({ volume, target, read_only }));
  }
  const portsText = draft.ports.trim();
  let ports;
  if (portsText) {
    try { ports = JSON.parse(portsText); } catch { throw new Error('Ports must be valid JSON, such as [{"container":8080,"host":18080,"protocol":"tcp"}].'); }
    const allowed = new Set(['container', 'host', 'protocol']);
    const validPort = (value) => Number.isInteger(value) && value >= 1 && value <= 65_535;
    if (!Array.isArray(ports) || ports.length > 64
      || ports.some((port) => !port || typeof port !== 'object' || Array.isArray(port)
        || Object.keys(port).some((key) => !allowed.has(key))
        || !validPort(port.container) || (port.host !== undefined && port.host !== null && !validPort(port.host))
        || !['tcp', 'udp'].includes(port.protocol))
      || new Set(ports.map((port) => `${port.container}/${port.protocol}`)).size !== ports.length) {
      throw new Error('Ports must contain at most 64 unique container-port/protocol pairs from 1 to 65535; host is an optional port number, not an address.');
    }
    ports = ports.map(({ container, host = null, protocol }) => ({ container, host, protocol }));
  }
  return {
    ...(hostname ? { hostname } : {}),
    ...(entrypoint ? { entrypoint } : {}),
    ...(command ? { command } : {}),
    ...(environment ? { environment } : {}),
    ...(workingDirectory ? { working_directory: workingDirectory } : {}),
    ...(user ? { user } : {}),
    ...(labels ? { labels } : {}),
    ...(network ? { network } : {}),
    ...(memoryMb === null ? {} : { memory_mb: memoryMb }),
    ...(cpus === null ? {} : { cpus }),
    ...(pidsLimit === null ? {} : { pids_limit: pidsLimit }),
    ...(mounts ? { mounts } : {}),
    ...(ports ? { ports } : {}),
  };
}

function optionalDecimalLimit(value, label, maximum) {
  const text = value.trim();
  if (!text) return null;
  if (!/^[0-9]+$/.test(text)) throw new Error(`${label} must be a whole decimal number from 1 to ${maximum}.`);
  const parsed = Number(text);
  if (!Number.isSafeInteger(parsed) || parsed < 1 || parsed > maximum) {
    throw new Error(`${label} must be a whole decimal number from 1 to ${maximum}.`);
  }
  return parsed;
}

export function Containers({ api, resource, containerDetails, onOpenExecution }) {
  const localDetails = useMemo(() => new ContainerDetailsSource(), []);
  const detailsSource = containerDetails ?? localDetails;
  const [selected, setSelected] = useState(null);
  const [busy, setBusy] = useState('');
  const [inspection, setInspection] = useState({ id: '', state: 'idle', count: 0, detail: null, error: null });
  const inspectionRevision = useRef(0);
  const inventoryRevision = useRef(resource.data);
  const [draft, setDraft] = useState({
    image: '', name: '', hostname: '', user: '', labels: '', network: '', entrypoint: '', command: '', environment: '', workingDirectory: '', memoryMb: '', cpus: '', pidsLimit: '', mounts: '', ports: '',
  });
  const [created, setCreated] = useState(null);
  const [creationError, setCreationError] = useState(null);
  const [creationNotice, setCreationNotice] = useState('');
  const currentContainers = useRef(new Map());
  currentContainers.current = new Map((resource.data ?? []).map((container) => [container.id, container.state]));
  const act = async (verb, id, ...args) => {
    setBusy(`${verb}:${id}`);
    try { await api.containers[verb](id, ...args); await resource.reload(); } finally { setBusy(''); }
  };
  const inspect = async (item) => {
    const revision = ++inspectionRevision.current;
    setSelected(item.id);
    setInspection({ id: item.id, state: 'loading', count: 0, detail: null, error: null });
    try {
      const detail = await api.containers.inspect(item.id);
      if (revision !== inspectionRevision.current) return;
      const count = await detailsSource.replace(detail);
      if (revision !== inspectionRevision.current) return;
      setInspection({ id: item.id, state: 'ready', count, detail, error: null });
    } catch (cause) {
      if (revision === inspectionRevision.current) setInspection({ id: item.id, state: 'error', count: 0, detail: null, error: cause });
    }
  };
  useEffect(() => {
    if (inventoryRevision.current === resource.data) return;
    inventoryRevision.current = resource.data;
    inspectionRevision.current += 1;
    setSelected(null);
    setInspection({ id: '', state: 'idle', count: 0, detail: null, error: null });
  }, [resource.data]);
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
        const id = await api.containers.create({
          image: draft.image.trim(), name: draft.name.trim(), ...containerCreateOptions(draft),
        });
        target = { id, name: draft.name.trim() };
        setCreated(target);
      }
      await api.containers.start(target.id);
      setCreationNotice(`Created and started ${target.name}.`);
      setCreated(null); setDraft({
        image: '', name: '', hostname: '', user: '', labels: '', network: '', entrypoint: '', command: '', environment: '', workingDirectory: '', memoryMb: '', cpus: '', pidsLimit: '', mounts: '', ports: '',
      });
      await resource.reload();
    } catch (cause) { setCreationError(cause); } finally { setBusy(''); }
  };
  let configurationError = '';
  try { containerCreateOptions(draft); } catch (error) { configurationError = error.message; }
  const remove = async (item) => {
    if (currentContainers.current.get(item.id) !== 'stopped' || item.state !== 'stopped') {
      throw new Error(`Container ${item.id} changed or is no longer stopped; refresh and confirm again.`);
    }
    setBusy(`remove:${item.id}`);
    try {
      await api.containers.remove(item.id);
      inspectionRevision.current += 1;
      setSelected(null);
      setInspection({ id: '', state: 'idle', count: 0, detail: null, error: null });
      await detailsSource.replace(null);
      await resource.reload();
    } finally { setBusy(''); }
  };
  const view = bounded(resource.data);
  const state = resource.loading ? 'loading' : resource.error ? 'error' : view.records.length === 0 ? 'empty' : 'ready';
  return (
    <Page
      title={'Containers'}
      subtitle={'Lifecycle, process inspection, logs, and execution.'}>
      <Card variant={'outline'}>
        <CardHeader
          label={'Create a container'}
          detail={'Uses a local image and starts it after durable creation.'} />
        <CardContent gap={1}>
          <Heading label={'Identity and image'} scale={'body'} />
          <Row gap={1} wrap={true}>
            <Entry
              value={draft.image}
              placeholder={'Image reference'}
              enabled={!created && busy !== 'create'}
              onChange={(event) => setDraft((value) => ({ ...value, image: String(event.value ?? '') }))} />
            <Entry
              value={draft.name}
              placeholder={'Container name'}
              enabled={!created && busy !== 'create'}
              onChange={(event) => setDraft((value) => ({ ...value, name: String(event.value ?? '') }))} />
            <Entry
              value={draft.hostname}
              placeholder={'Hostname (optional)'}
              enabled={!created && busy !== 'create'}
              onChange={(event) => setDraft((value) => ({ ...value, hostname: String(event.value ?? '') }))} />
            <Entry
              value={draft.user}
              placeholder={'Run as user (optional)'}
              enabled={!created && busy !== 'create'}
              onChange={(event) => setDraft((value) => ({ ...value, user: String(event.value ?? '') }))} />
            <Entry
              value={draft.labels}
              placeholder={'Labels JSON (optional)'}
              enabled={!created && busy !== 'create'}
              onChange={(event) => setDraft((value) => ({ ...value, labels: String(event.value ?? '') }))} />
          </Row>
          <Text
            label={'Labels use JSON [name, value] pairs, for example [["role","worker"]].'}
            color={'text-dim'}
            wrap={true} />
          <Heading label={'Process'} scale={'body'} />
          <Row gap={1} wrap={true}>
            <Entry
              value={draft.entrypoint}
              placeholder={'Entrypoint argv JSON (optional)'}
              enabled={!created && busy !== 'create'}
              onChange={(event) => setDraft((value) => ({ ...value, entrypoint: String(event.value ?? '') }))} />
            <Entry
              value={draft.command}
              placeholder={'Command argv JSON (optional)'}
              enabled={!created && busy !== 'create'}
              onChange={(event) => setDraft((value) => ({ ...value, command: String(event.value ?? '') }))} />
            <Entry
              value={draft.environment}
              placeholder={'Environment pairs JSON (optional)'}
              enabled={!created && busy !== 'create'}
              onChange={(event) => setDraft((value) => ({ ...value, environment: String(event.value ?? '') }))} />
            <Entry
              value={draft.workingDirectory}
              placeholder={'Working directory (optional)'}
              enabled={!created && busy !== 'create'}
              onChange={(event) => setDraft((value) => ({ ...value, workingDirectory: String(event.value ?? '') }))} />
          </Row>
          <Text
            label={'Entrypoint and command use JSON argv arrays; environment uses JSON [name, value] pairs.'}
            color={'text-dim'}
            wrap={true} />
          <Heading label={'Resources and connectivity'} scale={'body'} />
          <Row gap={1} wrap={true}>
            <Entry
              value={draft.memoryMb}
              placeholder={'Memory limit MiB (optional)'}
              enabled={!created && busy !== 'create'}
              onChange={(event) => setDraft((value) => ({ ...value, memoryMb: String(event.value ?? '') }))} />
            <Entry
              value={draft.cpus}
              placeholder={'CPU limit (optional)'}
              enabled={!created && busy !== 'create'}
              onChange={(event) => setDraft((value) => ({ ...value, cpus: String(event.value ?? '') }))} />
            <Entry
              value={draft.pidsLimit}
              placeholder={'PID limit (optional)'}
              enabled={!created && busy !== 'create'}
              onChange={(event) => setDraft((value) => ({ ...value, pidsLimit: String(event.value ?? '') }))} />
            <Entry
              value={draft.network}
              placeholder={'Initial network (optional)'}
              enabled={!created && busy !== 'create'}
              onChange={(event) => setDraft((value) => ({ ...value, network: String(event.value ?? '') }))} />
            <Entry
              value={draft.mounts}
              placeholder={'Named volume mounts JSON (optional)'}
              enabled={!created && busy !== 'create'}
              onChange={(event) => setDraft((value) => ({ ...value, mounts: String(event.value ?? '') }))} />
            <Entry
              value={draft.ports}
              placeholder={'Published ports JSON (optional)'}
              enabled={!created && busy !== 'create'}
              onChange={(event) => setDraft((value) => ({ ...value, ports: String(event.value ?? '') }))} />
          </Row>
          <Text
            label={'Mounts and ports use JSON object arrays; host filesystem paths and host addresses are not accepted.'}
            color={'text-dim'}
            wrap={true} />
        </CardContent>
        <CardActions>
          {busy === 'create' ? <Spinner /> : null}
          <Button
            label={created ? 'Retry start' : busy === 'create' ? 'Creating…' : 'Create and start'}
            enabled={busy === '' && (created !== null || (draft.image.trim().length > 0 && draft.name.trim().length > 0 && !configurationError))}
            onInvoke={createAndStart} />
        </CardActions>
        {configurationError ? <Text label={configurationError} color={'danger'} wrap={true} /> : null}
        <ErrorText error={creationError} />
        {creationNotice ? <Text label={creationNotice} color={'positive'} wrap={true} /> : null}
      </Card>
      <Toolbar loading={resource.loading} onRefresh={resource.reload} />
      <ResourceState
        state={state}
        loadingLabel={'Reading containers…'}
        emptyLabel={'No containers'}
        emptyDetail={'Create a container through an agent or extension, then refresh this page.'}
        error={resource.error?.message ?? String(resource.error ?? '')}
        retryLabel={'Retry containers'}
        onRetry={resource.reload}>
        {view.records.map((item) => <Card key={item.id} variant={selected === item.id ? 'filled' : 'outline'}>
          <CardHeader label={item.name || shortId(item.id)} detail={item.image} />
          <CardContent gap={2}>
            <Row gap={2} align={'center'}>
              <Badge label={item.state} tone={stateTone(item.state)} />
              <Text label={shortId(item.id)} color={'text-dim'} />
            </Row>
            <ContainerRename api={api} container={item} reload={resource.reload} blocked={busy !== ''} />
          </CardContent>
          <CardActions gap={1}>
            <Button
              label={selected === item.id ? 'Hide details' : 'Details'}
              onInvoke={() => toggleDetails(item)} />
            {containerActions(item, busy, act, remove)}
          </CardActions>
          {selected === item.id ? <ContainerDetail
            api={api}
            container={item}
            act={act}
            inspection={inspection}
            onRetry={() => inspect(item)}
            onOpenExecution={onOpenExecution} /> : null}
        </Card>)}
        <Omitted count={view.omitted} />
      </ResourceState>
    </Page>
  );
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
  return (
    <Column gap={1}>
      <Heading label={'Rename container'} scale={'caption'} />
      <Text
        label={`Current name: ${current || '(unnamed)'}. Immutable ID: ${container.id}`}
        color={'text-dim'}
        wrap={true} />
      <Row gap={1} wrap={true} align={'center'}>
        <Entry
          value={draft}
          placeholder={`New name for ${shortId(container.id)}`}
          enabled={!blocked && result.state !== 'loading'}
          onChange={(event) => {
            setDraft(String(event.value ?? '').slice(0, 129));
            setResult({ state: 'idle', error: null, name: '' });
          }} />
        {result.state === 'loading' ? <Spinner /> : null}
        <Button
          label={result.state === 'loading' ? 'Renaming…' : result.state === 'error' ? 'Retry rename' : 'Rename'}
          enabled={!blocked && result.state !== 'loading' && changed && !validation}
          onInvoke={rename} />
      </Row>
      {changed && validation ? <Text label={validation} color={'danger'} wrap={true} /> : null}
      {result.state === 'error' ? <Text
        label={result.error?.message ?? String(result.error)}
        color={'danger'}
        wrap={true} /> : null}
      {result.state === 'success' ? <Text
        label={`Renamed to ${result.name}. Inventory identity will update after the authoritative refresh.`}
        color={'positive'}
        wrap={true} /> : null}
    </Column>
  );
}

function containerActions(item, busy, act, remove) {
  const blocked = busy !== '';
  const running = item.state === 'running';
  return [
    <Button
      key={'start'}
      label={running ? 'Restart' : 'Start'}
      enabled={!blocked}
      onInvoke={() => act(running ? 'restart' : 'start', item.id)} />,
    <Button
      key={'pause'}
      label={item.state === 'paused' ? 'Resume' : 'Pause'}
      enabled={!blocked && (running || item.state === 'paused')}
      onInvoke={() => act(item.state === 'paused' ? 'unpause' : 'pause', item.id)} />,
    <ConfirmAction
      key={'stop'}
      label={'Stop'}
      confirmLabel={'Confirm stop'}
      pendingLabel={'Confirm stop'}
      authorityKey={`container:${item.id}:stop`}
      question={`Stop ${item.name || shortId(item.id)} with immutable ID ${item.id}?`}
      enabled={!blocked && running}
      onConfirm={() => act('stop', item.id)} />,
    <ConfirmAction
      key={'remove'}
      label={'Remove'}
      confirmLabel={'Confirm remove'}
      pendingLabel={'Confirm remove'}
      authorityKey={`container:${item.id}:remove`}
      question={`Remove stopped container ${item.name || shortId(item.id)} with immutable ID ${item.id}?`}
      enabled={!blocked && item.state === 'stopped'}
      onConfirm={() => remove(item)} />,
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
  return (
    <CardContent gap={2}>
      <ResourceState
        state={inspection.state === 'idle' ? 'loading' : inspection.state === 'ready' && inspection.count === 0 ? 'empty' : inspection.state}
        loadingLabel={'Reading container details…'}
        emptyLabel={'No container details'}
        emptyDetail={'The host returned no inspectable fields.'}
        error={inspection.error?.message ?? String(inspection.error ?? '')}
        retryLabel={'Retry details'}
        onRetry={onRetry}>
        <StructuredDetail value={inspection.detail} />
      </ResourceState>
      <Separator />
      <Heading label={'Quick actions'} scale={'caption'} />
      <Row gap={1} wrap={true}>
        <Button label={'Load logs'} onInvoke={readLogs} />
        <ConfirmAction
          authorityKey={`container:${container.id}:kill:SIGKILL`}
          label={'Kill'}
          confirmLabel={'Confirm kill'}
          pendingLabel={'Confirm kill'}
          question={`Force-kill ${container.name || shortId(container.id)} with immutable ID ${container.id}?`}
          onConfirm={() => act('kill', container.id, 'SIGKILL')} />
      </Row>
      {logs === null ? null : <Text label={logs || 'No log output.'} wrap={true} />}
      <Separator />
      <Heading label={'Captured execution'} scale={'caption'} />
      <Text
        label={'Runs without an interactive terminal. Inspect the resulting record for status and captured stdout/stderr.'}
        color={'text-dim'}
        wrap={true} />
      <Text
        label={'Enter an argument array so spaces and quoting remain exact, for example ["sh","-lc","printf hello"].'}
        color={'text-dim'}
        wrap={true} />
      <Row gap={1} wrap={true}>
        <Entry
          value={command.argv}
          placeholder={'Command argv JSON'}
          enabled={execution.state !== 'loading'}
          onChange={(event) => setCommand((value) => ({ ...value, argv: String(event.value ?? '') }))} />
        <Entry
          value={command.user}
          placeholder={'Run as user (optional)'}
          enabled={execution.state !== 'loading'}
          onChange={(event) => setCommand((value) => ({ ...value, user: String(event.value ?? '') }))} />
        <Entry
          value={command.workingDirectory}
          placeholder={'Working directory (optional)'}
          enabled={execution.state !== 'loading'}
          onChange={(event) => setCommand((value) => ({ ...value, workingDirectory: String(event.value ?? '') }))} />
      </Row>
      <Row gap={1} wrap={true}>
        <Button
          label={execution.state === 'loading' ? 'Executing…' : 'Execute'}
          enabled={execution.state !== 'loading' && command.argv.trim().length > 0}
          onInvoke={run} />
        <Button
          label={attachment.state === 'loading' ? 'Attaching…' : 'Attach terminal'}
          enabled={attachment.state !== 'loading' && command.argv.trim().length > 0 && container.state === 'running'}
          onInvoke={attach} />
      </Row>
      {attachment.state === 'error' ? <Text
        label={attachment.error?.message ?? String(attachment.error)}
        color={'danger'}
        wrap={true} /> : null}
      {attachment.state === 'ready' ? <Text
        label={`Interactive terminal opened in ${attachment.slot}.`}
        color={'positive'}
        wrap={true} /> : null}
      {execution.state === 'error' ? <Text
        label={execution.error?.message ?? String(execution.error)}
        color={'danger'}
        wrap={true} /> : null}
      {execution.state === 'ready' ? <Row gap={1} wrap={true} align={'center'}>
        <Text
          label={`Execution ${execution.id} created.`}
          color={'positive'}
          wrap={true} />
        {onOpenExecution ? <Button
          label={'Inspect execution'}
          onInvoke={() => onOpenExecution(execution.id)} /> : null}
      </Row> : null}
    </CardContent>
  );
}

export function Processes({ api, resource }) {
  const [snapshots, setSnapshots] = useState([]);
  const [failures, setFailures] = useState([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState(null);
  const loadRevision = useRef(0);
  const load = useCallback(async () => {
    const revision = ++loadRevision.current;
    setLoading(true);
    try {
      const containers = resource.data ?? [];
      const groups = new Array(containers.length);
      let cursor = 0;
      const worker = async () => {
        while (cursor < containers.length) {
          const index = cursor; cursor += 1;
          const container = containers[index];
          try { groups[index] = { container, rows: await api.containers.processes(container.id), error: null }; }
          catch (cause) { groups[index] = { container, rows: null, error: cause }; }
        }
      };
      await Promise.all(Array.from({ length: Math.min(PROCESS_SAMPLING_CONCURRENCY, containers.length) }, worker));
      if (revision !== loadRevision.current) return;
      const available = groups.filter(({ rows }) => rows !== null);
      const unavailable = groups.filter(({ error: cause }) => cause !== null);
      setSnapshots(available);
      setFailures(unavailable);
      setError(available.length === 0 && unavailable.length > 0 ? unavailable[0].error : null);
    } finally { if (revision === loadRevision.current) setLoading(false); }
  }, [api, resource.data]);
  useEffect(() => {
    void load();
    return () => { loadRevision.current += 1; };
  }, [load]);
  const processes = snapshots.flatMap(({ container, rows }) => processRows(rows, container.name || shortId(container.id)));
  const observed = Math.max(0, ...snapshots.map(({ rows }) => Number(rows.observed_at_ms) || 0));
  const completeNamespace = snapshots.length > 0 && snapshots.every(({ rows }) => rows.scope === 'namespace');
  const view = bounded(processes);
  const failure = error ?? resource.error;
  const state = loading || resource.loading ? 'loading' : failure ? 'error' : view.records.length === 0 ? 'empty' : 'ready';
  return (
    <Page
      title={'Processes'}
      subtitle={'A bounded snapshot across all visible containers.'}>
      <Toolbar loading={state === 'loading'} onRefresh={load} />
      <ResourceState
        state={state}
        loadingLabel={'Reading processes…'}
        emptyLabel={'No running processes'}
        emptyDetail={'Start a container to see its process snapshot here.'}
        error={failure?.message ?? String(failure ?? '')}
        retryLabel={'Retry processes'}
        onRetry={resource.error ? resource.reload : load}>
        <Text
          label={completeNamespace
            ? 'Full container namespace snapshots; PIDs identify only this observation and may be reused.'
            : 'Initial processes only; PIDs identify this snapshot and may be reused.'}
          color={'text-dim'}
          wrap={true} />
        {observed > 0 ? <Text label={`Observed ${new Date(observed).toISOString()}`} color={'text-dim'} /> : null}
        {view.records.map((process, index) => {
          const pid = process.cells.PID ?? process.cells.Pid ?? process.cells.pid ?? '—';
          const command = process.cells.CMD ?? process.cells.Command ?? process.cells.COMMAND ?? process.values.at(-1) ?? 'Process';
          const detail = Object.entries(process.cells).filter(([key]) => !['PID', 'Pid', 'pid', 'CMD', 'Command', 'COMMAND'].includes(key)).map(([key, value]) => `${key} ${value}`).join(' · ');
          return (
            <Card key={`${process.container}:${pid}:${index}`} variant={'outline'}>
              <CardHeader label={command} detail={process.container} />
              <CardContent>
                <Row gap={2}>
                  <Badge label={`PID ${pid}`} />
                  <Text label={detail} color={'text-dim'} />
                </Row>
              </CardContent>
            </Card>
          );
        })}
        <Omitted count={view.omitted} />
        {snapshots.some(({ rows }) => rows.truncated)
          ? <Text
          label={'The host process snapshot was truncated at its safety limit.'}
          color={'warning'}
          wrap={true} /> : null}
      </ResourceState>
      {snapshots.length > 0 && failures.length > 0 ? <Column gap={1}>
        <Text
          label={`${failures.length} container process snapshot${failures.length === 1 ? '' : 's'} unavailable; available containers remain visible.`}
          color={'warning'}
          wrap={true} />
        {failures.slice(0, 8).map(({ container, error: cause }) => <Text
          key={container.id}
          label={`${container.name || shortId(container.id)}: ${String(cause?.message ?? cause).slice(0, 256)}`}
          color={'text-dim'}
          wrap={true} />)}
        {failures.length > 8 ? <Text
          label={`${failures.length - 8} more failures omitted.`}
          color={'text-dim'} /> : null}
      </Column> : null}
    </Page>
  );
}

export function Executions({ api, resource, executionDetails, truncated = false, requestedExecution = '' }) {
  const localDetails = useMemo(() => new ExecutionDetailsSource(), []);
  const detailsSource = executionDetails ?? localDetails;
  const [selected, setSelected] = useState('');
  const [inspection, setInspection] = useState({ state: 'idle', count: 0, error: null });
  const [output, setOutput] = useState(null);
  const [busy, setBusy] = useState('');
  const [inventoryVersion, setInventoryVersion] = useState(0);
  const lifecycleRevision = useRef(0);
  const inventoryRevision = useRef(resource.data);
  const inspect = async (id) => {
    const revision = ++lifecycleRevision.current;
    setSelected(id); setInspection({ state: 'loading', count: 0, error: null }); setOutput(null);
    try {
      const detail = await api.containers.execution(id);
      if (revision !== lifecycleRevision.current) return;
      const count = await detailsSource.replace(detail);
      if (revision !== lifecycleRevision.current) return;
      setInspection({ state: 'ready', count, error: null });
    } catch (error) { if (revision === lifecycleRevision.current) setInspection({ state: 'error', count: 0, error }); }
  };
  useEffect(() => {
    if (requestedExecution && selected !== requestedExecution) void inspect(requestedExecution);
  }, [requestedExecution]);
  const logs = async (id) => {
    const revision = lifecycleRevision.current;
    setBusy(`logs:${id}`);
    try {
      const value = await api.containers.executionLogs(id, { stdout: true, stderr: true });
      if (revision !== lifecycleRevision.current) return;
      const text = (bytes) => logText(bytes).slice(-LOG_VIEW_CHARACTER_LIMIT);
      setOutput((current) => ({ revision: (current?.revision ?? 0) + 1,
        stdout: text({ stdout: value.stdout, stderr: [] }), stderr: text({ stdout: [], stderr: value.stderr }),
        truncated: value.truncated, stdoutTruncated: value.stdout_truncated, stderrTruncated: value.stderr_truncated,
        eof: value.eof }));
    } finally { if (revision === lifecycleRevision.current) setBusy(''); }
  };
  const wait = async (id) => {
    const revision = lifecycleRevision.current;
    setBusy(`wait:${id}`);
    try { const detail = await api.containers.waitExecution(id, { timeoutMs: 5_000 }); if (revision !== lifecycleRevision.current) return; await detailsSource.replace(detail); if (revision === lifecycleRevision.current) await resource.reload(); }
    finally { if (revision === lifecycleRevision.current) setBusy(''); }
  };
  const terminate = async (id) => {
    await api.containers.signalExecution(id, 'SIGTERM');
    await resource.reload();
    await inspect(id);
  };
  const remove = async (id) => { await api.containers.removeExecution(id); setSelected(''); setOutput(null); await resource.reload(); };
  useEffect(() => {
    if (inventoryRevision.current === resource.data) return;
    inventoryRevision.current = resource.data;
    lifecycleRevision.current += 1;
    setSelected(''); setInspection({ state: 'idle', count: 0, error: null }); setOutput(null); setBusy('');
    setInventoryVersion((version) => version + 1);
  }, [resource.data]);
  const view = bounded(resource.data);
  const state = resource.loading ? 'loading' : resource.error ? 'error' : view.records.length === 0 ? 'empty' : 'ready';
  return (
    <Page
      title={'Executions'}
      subtitle={'Bounded exec-session catalogue, status and captured output.'}>
      <Toolbar loading={resource.loading} onRefresh={resource.reload} />
      <ResourceState
        state={state}
        loadingLabel={'Reading executions…'}
        emptyLabel={'No executions'}
        emptyDetail={'Commands executed in containers will appear here.'}
        error={resource.error?.message ?? String(resource.error ?? '')}
        retryLabel={'Retry executions'}
        onRetry={resource.reload}>
        {view.records.map((item) => <Card
          key={`${inventoryVersion}:${item.id}`}
          variant={selected === item.id ? 'filled' : 'outline'}>
          <CardHeader
            label={item.command?.join(' ') || shortId(item.id)}
            detail={`container ${shortId(item.container_id)}`} />
          <CardContent>
            <Badge
              label={item.running ? 'running' : `exited ${item.exit_code}`}
              tone={item.running ? 'positive' : 'neutral'} />
            {selected !== item.id ? null : <ResourceState
              state={inspection.state === 'idle' ? 'loading' : inspection.state === 'ready' && inspection.count === 0 ? 'empty' : inspection.state}
              loadingLabel={'Reading execution details…'}
              emptyLabel={'No execution details'}
              emptyDetail={'The host returned no inspectable fields.'}
              error={inspection.error?.message ?? String(inspection.error ?? '')}
              retryLabel={'Retry details'}
              onRetry={() => inspect(item.id)}>
              <KeyValueTable
                source={EXECUTION_DETAIL_SOURCE}
                schema={IMAGE_DETAIL_SCHEMA}
                height={{ minimum: { step: 10 }, maximum: { step: 28 } }} />
            </ResourceState>}
            {selected === item.id && output ? <Column gap={1}>
              <Heading label={'Standard output'} scale={'caption'} />
              <LogView
                key={`stdout-${output.revision}`}
                value={output.stdout || (output.eof ? 'No stdout captured (EOF).' : 'No stdout captured yet; execution is still running.')}
                monospace={true} />
              {output.stdoutTruncated ? <Text
                label={'Standard output was truncated to its configured bound.'}
                color={'warning'} /> : null}
              <Heading label={'Standard error'} scale={'caption'} />
              <LogView
                key={`stderr-${output.revision}`}
                value={output.stderr || (output.eof ? 'No stderr captured (EOF).' : 'No stderr captured yet; execution is still running.')}
                monospace={true} />
              {output.stderrTruncated ? <Text
                label={'Standard error was truncated to its configured bound.'}
                color={'warning'} /> : null}
              {output.eof ? <Text label={'Captured output is complete (EOF).'} color={'text-dim'} />
                : <Text
                label={'Execution is still running; later output may appear.'}
                color={'text-dim'} />}
              {output.truncated && !output.stdoutTruncated && !output.stderrTruncated
                ? <Text
                label={'Host output was truncated to its configured bound.'}
                color={'warning'} /> : null}
            </Column> : null}
          </CardContent>
          <CardActions gap={1}>
            <Button
              label={selected === item.id ? 'Hide details' : 'Details'}
              enabled={!busy}
              onInvoke={() => selected === item.id ? setSelected('') : void inspect(item.id)} />
            <Button
              label={busy === `logs:${item.id}` ? 'Loading logs…' : 'Load output'}
              enabled={!busy}
              onInvoke={() => void logs(item.id)} />
            <Button
              label={busy === `wait:${item.id}` ? 'Waiting…' : 'Wait up to 5s'}
              enabled={!busy && item.running}
              onInvoke={() => void wait(item.id)} />
            <ConfirmAction
              authorityKey={`execution:${item.id}:SIGTERM`}
              label={'Terminate'}
              confirmLabel={'Confirm SIGTERM'}
              pendingLabel={'Confirm SIGTERM'}
              question={`Send SIGTERM to execution ${item.id}?`}
              enabled={!busy && item.running}
              onConfirm={() => terminate(item.id)} />
            <ConfirmAction
              authorityKey={`execution:${item.id}:remove`}
              label={'Remove record'}
              confirmLabel={'Confirm removal'}
              pendingLabel={'Confirm removal'}
              question={`Remove execution record ${shortId(item.id)}?`}
              enabled={!busy && !item.running}
              onConfirm={() => remove(item.id)} />
          </CardActions>
        </Card>)}
        <Omitted count={view.omitted} />
        {truncated ? <Text
          label={'The host execution catalogue was truncated at its safety limit.'}
          color={'warning'}
          wrap={true} /> : null}
      </ResourceState>
    </Page>
  );
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
  const inspectionRevision = useRef(0);
  const inventoryRevision = useRef(resource.data);
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
      if (disposed || status.job !== pull.job || status.revision < change.revision) return;
      setPull(status);
      if (status.state === 'complete') { setNotice(`Pulled ${status.reference}.`); await resource.reload(); }
    }).then((dispose) => { if (disposed) void dispose(); else stop = dispose; }).catch((error) => {
      if (!disposed) setPull((current) => ({ ...current, state: 'failed', error: error.message ?? String(error) }));
    });
    return () => { disposed = true; if (stop) void stop(); };
  }, [api, pull?.job, pull?.revision, pull?.state, resource.reload]);
  const cancelPull = () => run('pull-cancel', async () => {
    await api.images.cancelPull(pull.job);
    setPull((current) => ({ ...current, state: 'cancelled', status: 'Pull cancelled.' }));
  });
  const inspect = async (item) => {
    const revision = ++inspectionRevision.current;
    setBusy(`inspect:${item.id}`);
    setDetail(null);
    setInspection({ id: item.id, state: 'loading', count: 0, error: null });
    try {
      const value = await api.images.inspect(item.reference || item.id);
      if (revision !== inspectionRevision.current) return;
      const count = await detailsSource.replace(value);
      if (revision !== inspectionRevision.current) return;
      setDetail(value);
      setInspection({ id: item.id, state: 'ready', count, error: null });
    } catch (cause) {
      if (revision === inspectionRevision.current) setInspection({ id: item.id, state: 'error', count: 0, error: cause });
    } finally {
      setBusy('');
    }
  };
  useEffect(() => {
    if (inventoryRevision.current === resource.data) return;
    inventoryRevision.current = resource.data;
    inspectionRevision.current += 1;
    setDetail(null);
    setInspection({ id: '', state: 'idle', count: 0, error: null });
    setConfirm('');
  }, [resource.data]);
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
  const inventoryState = resource.loading ? 'loading' : resource.error ? 'error' : view.records.length === 0 ? 'empty' : 'ready';
  return (
    <Page title={'Images'} subtitle={'Images available to this workspace.'}>
      <Row gap={1}>
        <Entry
          value={reference}
          placeholder={'registry/image:tag'}
          onChange={(event) => setReference(String(event.value ?? ''))} />
        <Button
          label={pull?.state === 'failed' ? 'Retry pull' : busy === 'pull' ? 'Starting…' : 'Pull'}
          enabled={!busy && reference.trim().length > 0 && (!pull || ['complete', 'failed', 'cancelled'].includes(pull.state))}
          onInvoke={startPull} />
        <Button label={'Refresh'} enabled={!busy} onInvoke={resource.reload} />
      </Row>
      {pull ? <Card variant={pull.state === 'failed' ? 'outline' : 'filled'}>
        <CardHeader label={pull.reference} detail={pull.status ?? pull.state} />
        <CardContent gap={1}>
          {pull.total > 0 ? <Meter
            fraction={Math.min(1, pull.current / pull.total)}
            value={`${pull.current} / ${pull.total} bytes`} /> : pull.state === 'pulling' || pull.state === 'starting' ? <Spinner /> : null}
          {pull.layer ? <Text label={`Layer ${pull.layer}`} color={'text-dim'} /> : null}
          {pull.error ? <Text label={pull.error} color={'danger'} wrap={true} /> : null}
        </CardContent>
        <CardActions>
          {!['complete', 'failed', 'cancelled'].includes(pull.state) ? <Button label={'Cancel pull'} onInvoke={cancelPull} /> : null}
        </CardActions>
      </Card> : null}
      <ErrorText error={error} />
      {notice ? <Text label={notice} color={'positive'} /> : null}
      <ResourceState
        state={inventoryState}
        loadingLabel={'Reading images…'}
        emptyLabel={'No images'}
        emptyDetail={'Enter an image reference above to pull one into this workspace.'}
        error={resource.error?.message ?? String(resource.error ?? '')}
        retryLabel={'Retry images'}
        onRetry={resource.reload}>
        <Row gap={1} align={'center'}>
          {busy ? <Spinner /> : null}
          {confirm === 'prune'
            ? <React.Fragment>
            <Text label={'Remove every unused image?'} color={'warning'} />
            <Button
              label={'Confirm prune'}
              enabled={!busy}
              tone={'danger'}
              destructive={true}
              onInvoke={prune} />
            <Button label={'Cancel'} enabled={!busy} onInvoke={() => setConfirm('')} />
          </React.Fragment>
            : <Button
            label={'Prune unused images'}
            enabled={!busy}
            tone={'danger'}
            onInvoke={() => setConfirm('prune')} />}
        </Row>
        {view.records.map((item) => <Card key={item.id} variant={detail?.id === item.id ? 'filled' : 'outline'}>
          <CardHeader
            label={item.reference || item.repo_tags?.[0] || '<untagged>'}
            detail={shortId(item.id)} />
          <CardContent>
            <Text label={bytes(item.size)} color={'text-dim'} />
            {inspection.id === item.id ? <ResourceState
              state={inspection.state === 'ready' && inspection.count === 0 ? 'empty' : inspection.state}
              loadingLabel={'Reading image details…'}
              emptyLabel={'No image details'}
              emptyDetail={'The host returned no inspectable fields.'}
              error={inspection.error?.message ?? String(inspection.error ?? '')}
              retryLabel={'Retry inspect'}
              onRetry={() => inspect(item)}>
              <StructuredDetail value={detail} />
            </ResourceState> : null}
          </CardContent>
          <CardActions gap={1}>
            <Button label={'Inspect'} enabled={!busy} onInvoke={() => inspect(item)} />
            {confirm === item.id
              ? <React.Fragment>
              <Text label={`Remove immutable image ${item.id}?`} color={'warning'} />
              <Button
                label={'Confirm remove'}
                enabled={!busy}
                tone={'danger'}
                destructive={true}
                onInvoke={() => remove(item)} />
              <Button label={'Cancel'} enabled={!busy} onInvoke={() => setConfirm('')} />
            </React.Fragment>
              : <Button
              label={'Remove'}
              enabled={!busy}
              tone={'danger'}
              onInvoke={() => setConfirm(item.id)} />}
          </CardActions>
        </Card>)}
        <Omitted count={view.omitted} />
      </ResourceState>
    </Page>
  );
}

export function Volumes({ api, resource, volumeDetails }) {
  const localDetails = useMemo(() => new VolumeDetailsSource(), []);
  const detailsSource = volumeDetails ?? localDetails;
  const [name, setName] = useState('');
  const [inspection, setInspection] = useState({ name: '', state: 'idle', count: 0, detail: null, error: null });
  const [creation, setCreation] = useState({ state: 'idle', name: '', error: null });
  const inspectionRevision = useRef(0);
  const inventoryRevision = useRef(resource.data);
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
    const revision = ++inspectionRevision.current;
    setInspection({ name: volume.name, state: 'loading', count: 0, detail: null, error: null });
    try {
      const detail = await api.volumes.inspect(volume.name);
      if (revision !== inspectionRevision.current) return;
      const count = await detailsSource.replace(detail);
      if (revision !== inspectionRevision.current) return;
      setInspection({ name: volume.name, state: 'ready', count, detail, error: null });
    } catch (error) { if (revision === inspectionRevision.current) setInspection({ name: volume.name, state: 'error', count: 0, detail: null, error }); }
  };
  useEffect(() => {
    if (inventoryRevision.current === resource.data) return;
    inventoryRevision.current = resource.data;
    inspectionRevision.current += 1;
    setInspection({ name: '', state: 'idle', count: 0, detail: null, error: null });
  }, [resource.data]);
  const view = bounded(resource.data);
  const inventoryState = resource.loading ? 'loading' : resource.error ? 'error' : view.records.length === 0 ? 'empty' : 'ready';
  return (
    <Page
      title={'Volumes'}
      subtitle={'Bounded local volume inventory and safe, non-force lifecycle.'}>
      <Row gap={1}>
        <Entry
          value={name}
          placeholder={'Volume name'}
          enabled={creation.state !== 'loading'}
          onChange={(event) => { setName(String(event.value ?? '')); setCreation({ state: 'idle', name: '', error: null }); }} />
        <Button
          label={creation.state === 'loading' ? 'Creating…' : creation.state === 'error' ? 'Retry create' : 'Create'}
          enabled={creation.state !== 'loading' && name.trim().length > 0}
          onInvoke={() => { void create(); }} />
        <Button
          label={'Refresh'}
          enabled={creation.state !== 'loading'}
          onInvoke={resource.reload} />
      </Row>
      {creation.state === 'loading' ? <Row gap={1} align={'center'}>
        <Spinner />
        <Text label={`Creating volume ${creation.name}…`} />
      </Row> : null}
      {creation.state === 'error' ? <Text label={boundedMessage(creation.error)} color={'danger'} wrap={true} /> : null}
      {creation.state === 'success' ? <Text label={`Created volume ${creation.name}.`} color={'positive'} wrap={true} /> : null}
      <ResourceState
        state={inventoryState}
        loadingLabel={'Reading volumes…'}
        emptyLabel={'No volumes'}
        emptyDetail={'Create a named volume above when a workload needs durable storage.'}
        error={resource.error?.message ?? String(resource.error ?? '')}
        retryLabel={'Retry volumes'}
        onRetry={resource.reload}>
        {view.records.map((volume) => <Card
          key={`${volume.name}:${volume.generation}`}
          variant={inspection.name === volume.name ? 'filled' : 'outline'}>
          <CardHeader label={volume.name} detail={volume.driver} />
          <CardActions gap={1}>
            <Button
              label={inspection.name === volume.name && inspection.state === 'error' ? 'Retry inspect' : 'Inspect'}
              onInvoke={() => inspect(volume)} />
            <ConfirmAction
              authorityKey={`volume:${volume.name}:${volume.generation}:remove`}
              label={'Remove'}
              confirmLabel={'Confirm remove'}
              pendingLabel={'Confirm remove'}
              question={`Remove volume ${volume.name} generation ${volume.generation}?`}
              onConfirm={() => remove(volume)} />
          </CardActions>
          {inspection.name === volume.name ? <CardContent>
            {inspection.state === 'loading'
              ? <Row gap={1} align={'center'}>
              <Spinner />
              <Text label={'Reading volume details…'} />
            </Row>
              : inspection.state === 'error'
                ? <Text
              label={inspection.error?.message ?? String(inspection.error)}
              color={'danger'}
              wrap={true} />
                : inspection.count === 0
                  ? <EmptyState
              label={'No volume details'}
              detail={'The host returned no inspectable fields.'} />
                  : <StructuredDetail value={inspection.detail} />}
          </CardContent> : null}
        </Card>)}
        <Omitted count={view.omitted} />
      </ResourceState>
    </Page>
  );
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
  const inspectionRevision = useRef(0);
  const inventoryRevision = useRef(resource.data);
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
    const revision = ++inspectionRevision.current;
    setInspection({ id, state: 'loading', count: 0, detail: null, error: null });
    try {
      const detail = await api.networks.inspect(id);
      if (revision !== inspectionRevision.current) return;
      const count = await detailsSource.replace(detail);
      if (revision !== inspectionRevision.current) return;
      setInspection({ id, state: 'ready', count, detail, error: null });
    } catch (error) { if (revision === inspectionRevision.current) setInspection({ id, state: 'error', count: 0, detail: null, error }); }
  };
  useEffect(() => {
    if (inventoryRevision.current === resource.data) return;
    inventoryRevision.current = resource.data;
    inspectionRevision.current += 1;
    setInspection({ id: '', state: 'idle', count: 0, detail: null, error: null });
    setDisconnectRequest(null);
  }, [resource.data]);
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
  const inventoryState = resource.loading ? 'loading' : resource.error ? 'error' : view.records.length === 0 ? 'empty' : 'ready';
  return (
    <Page
      title={'Networks'}
      subtitle={'Bounded network inventory; attachment changes are accepted only for stopped containers.'}>
      <Row gap={1}>
        <Entry
          value={name}
          placeholder={'Network name'}
          enabled={creation.state !== 'loading'}
          onChange={(event) => { setName(String(event.value ?? '')); setCreation({ state: 'idle', name: '', error: null }); }} />
        <Button
          label={creation.state === 'loading' ? 'Creating…' : creation.state === 'error' ? 'Retry create' : 'Create'}
          enabled={creation.state !== 'loading' && name.trim().length > 0}
          onInvoke={() => { void create(); }} />
        <Button
          label={'Refresh'}
          enabled={creation.state !== 'loading'}
          onInvoke={resource.reload} />
      </Row>
      {creation.state === 'loading' ? <Row gap={1} align={'center'}>
        <Spinner />
        <Text label={`Creating network ${creation.name}…`} />
      </Row> : null}
      {creation.state === 'error' ? <Text label={boundedMessage(creation.error)} color={'danger'} wrap={true} /> : null}
      {creation.state === 'success' ? <Text
        label={`Created network ${creation.name}.`}
        color={'positive'}
        wrap={true} /> : null}
      <Entry
        value={container}
        placeholder={'Complete container ID'}
        enabled={operation.state !== 'loading'}
        onChange={(event) => { setContainer(String(event.value ?? '')); setOperation({ state: 'idle', request: null, error: null }); setDisconnectRequest(null); }} />
      <Entry
        value={aliases}
        placeholder={'Endpoint aliases (comma-separated, optional)'}
        enabled={operation.state !== 'loading'}
        onChange={(event) => { setAliases(String(event.value ?? '')); setOperation({ state: 'idle', request: null, error: null }); }} />
      {operation.state === 'loading' ? <Row gap={1} align={'center'}>
        <Spinner />
        <Text label={`${title(operation.request.verb)}ing immutable endpoint…`} />
      </Row> : null}
      {operation.state === 'error' ? <Row gap={1} wrap={true}>
        <Text label={boundedMessage(operation.error)} color={'danger'} wrap={true} />
        {operation.request ? <Button
          label={`Retry ${operation.request.verb}`}
          onInvoke={() => { void attach(operation.request).catch(() => {}); }} /> : null}
      </Row> : null}
      {operation.state === 'success' ? <Text
        label={`${operation.request.verb === 'connect' ? 'Connected' : 'Disconnected'} container ${operation.request.container} ${operation.request.verb === 'connect' ? 'to' : 'from'} network ${operation.request.network}${operation.request.aliases.length ? ` with ${operation.request.aliases.length} endpoint alias${operation.request.aliases.length === 1 ? '' : 'es'}` : ''}.`}
        color={'positive'}
        wrap={true} /> : null}
      <ErrorText error={error} />
      <ResourceState
        state={inventoryState}
        loadingLabel={'Reading networks…'}
        emptyLabel={'No networks'}
        emptyDetail={'Create a network above to connect workspace containers.'}
        error={resource.error?.message ?? String(resource.error ?? '')}
        retryLabel={'Retry networks'}
        onRetry={resource.reload}>
        {view.records.map((network) => <Card
          key={resourceReference(network)}
          variant={inspection.id === resourceReference(network) ? 'filled' : 'outline'}>
          <CardHeader label={network.name} detail={`${network.driver} · ${network.scope}`} />
          <CardActions gap={1}>
            <Button
              label={inspection.id === resourceReference(network) && inspection.state === 'error' ? 'Retry inspect' : 'Inspect'}
              onInvoke={() => inspect(network)} />
            <Button
              label={'Connect'}
              enabled={operation.state !== 'loading' && container.trim().length > 0}
              onInvoke={() => begin(network, 'connect')} />
            <Button
              label={'Disconnect'}
              enabled={operation.state !== 'loading' && container.trim().length > 0}
              tone={'danger'}
              onInvoke={() => begin(network, 'disconnect')} />
            <ConfirmAction
              authorityKey={`network:${resourceReference(network)}:remove`}
              label={'Remove'}
              confirmLabel={'Confirm remove'}
              pendingLabel={'Confirm remove'}
              question={`Remove immutable network ${resourceReference(network)} (${network.name})?`}
              onConfirm={() => remove(network)} />
          </CardActions>
          {disconnectRequest?.network === resourceReference(network) ? <CardContent>
            <Text
              label={`Disconnect immutable container ${disconnectRequest.container} from network ${disconnectRequest.network}?`}
              color={'warning'}
              wrap={true} />
            <Row gap={1}>
              <Button
                label={'Confirm disconnect'}
                enabled={operation.state !== 'loading'}
                tone={'danger'}
                destructive={true}
                onInvoke={() => { void attach(disconnectRequest).catch(() => {}); }} />
              <Button
                label={'Cancel'}
                enabled={operation.state !== 'loading'}
                onInvoke={() => setDisconnectRequest(null)} />
            </Row>
          </CardContent> : null}
          {inspection.id === resourceReference(network) ? <CardContent>
            {inspection.state === 'loading'
              ? <Row gap={1} align={'center'}>
              <Spinner />
              <Text label={'Reading network details…'} />
            </Row>
              : inspection.state === 'error'
                ? <Text
              label={inspection.error?.message ?? String(inspection.error)}
              color={'danger'}
              wrap={true} />
                : inspection.count === 0
                  ? <EmptyState
              label={'No network details'}
              detail={'The host returned no inspectable fields.'} />
                  : <StructuredDetail value={inspection.detail} />}
          </CardContent> : null}
        </Card>)}
        <Omitted count={view.omitted} />
      </ResourceState>
    </Page>
  );
}

export function Terminals({ api, resource }) {
  const [busy, setBusy] = useState('');
  const [error, setError] = useState(null);
  const [selected, setSelected] = useState('');
  const [readable, setReadable] = useState(null);
  const [input, setInput] = useState('');
  const paneRevision = useRef(0);
  const view = bounded(resource.data ?? []);
  const state = resource.loading ? 'loading' : resource.error ? 'error' : view.records.length === 0 ? 'empty' : 'ready';
  const pin = async (tab) => {
    setBusy(tab.id); setError(null);
    try {
      await api.terminal.pinTab(tab.id, !tab.pinned);
      await resource.reload();
    } catch (cause) {
      setError(cause);
    } finally {
      setBusy('');
    }
  };
  const focus = async (tab) => {
    const slot = tab.panes?.[0]?.slot;
    if (!slot) return;
    setBusy(tab.id); setError(null);
    try { await api.terminal.focus(slot); } catch (cause) { setError(cause); } finally { setBusy(''); }
  };
  const inspect = async (slot) => {
    const requested = ++paneRevision.current;
    setBusy(`read:${slot}`); setError(null);
    try {
      const next = await api.terminal.toText(slot, { lines: 200 });
      if (requested !== paneRevision.current) return;
      setReadable(next);
      setSelected(slot);
    } catch (cause) {
      if (requested === paneRevision.current) setError(cause);
    } finally {
      if (requested === paneRevision.current) setBusy('');
    }
  };
  const sendLine = async () => {
    if (readable?.kind !== 'terminal' || selected === '' || input === '') return;
    const { generation, revision } = readable.snapshot;
    const requested = ++paneRevision.current;
    setBusy(`write:${selected}`); setError(null);
    try {
      const result = await api.terminal.writeAndWait(selected, generation, revision, `${input}\n`, { lines: 200 });
      if (requested !== paneRevision.current) return;
      if (result.changed) setReadable(result.readable ?? { kind: 'terminal', text: result.after.lines.join('\n'), snapshot: result.after });
      setInput('');
    } catch (cause) {
      if (requested === paneRevision.current) setError(cause);
    } finally {
      if (requested === paneRevision.current) setBusy('');
    }
  };
  useEffect(() => {
    if (!selected) return;
    const present = (resource.data ?? []).some((tab) => (tab.panes ?? []).some((pane) => pane.slot === selected));
    if (present) return;
    paneRevision.current += 1;
    setSelected(''); setReadable(null); setInput('');
  }, [resource.data, selected]);
  return (
    <Page
      title={'Terminal tabs'}
      subtitle={'Inspect live pane occupancy and protect important tabs from accidental close.'}>
      <Toolbar loading={resource.loading} onRefresh={resource.reload} />
      <ErrorText error={error} />
      <ResourceState
        state={state}
        loadingLabel={'Reading terminal tabs…'}
        emptyLabel={'No terminal tabs'}
        emptyDetail={'Open a terminal tab to manage it here.'}
        error={resource.error?.message ?? String(resource.error ?? '')}
        retryLabel={'Retry terminal tabs'}
        onRetry={resource.reload}>
        {view.records.map((tab) => <Card key={tab.id} variant={tab.pinned ? 'filled' : 'outline'}>
          <CardHeader label={tab.title} detail={tab.id} />
          <CardContent gap={1}>
            <Row gap={1} align={'center'}>
              <Badge label={tab.pinned ? 'Pinned' : 'Unpinned'} tone={tab.pinned ? 'positive' : 'neutral'} />
              <Text label={`${tab.panes?.length ?? 0} pane${tab.panes?.length === 1 ? '' : 's'}`} color={'text-dim'} />
            </Row>
            {(tab.panes ?? []).map((pane) => <Row key={pane.slot} gap={1} align={'center'}>
              <Text
                label={`${pane.slot} · ${pane.occupant}${pane.provider ? ` · ${pane.provider.extension}/${pane.provider.provider}` : ''}`}
                color={'text-dim'} />
              <Button
                label={`${selected === pane.slot ? 'Refresh' : 'Inspect'} ${pane.slot}`}
                enabled={busy === ''}
                variant={'ghost'}
                onInvoke={() => { void inspect(pane.slot); }} />
            </Row>)}
            {selected && (tab.panes ?? []).some((pane) => pane.slot === selected) && readable ? <Card variant={'filled'}>
              <CardHeader
                label={readable.kind === 'terminal' ? `Terminal ${selected}` : `Interface ${selected}`}
                detail={readable.kind === 'terminal' ? 'Bounded live screen text' : 'Bounded semantic XML'} />
              <CardContent gap={1}>
                <LogView value={readable.text.slice(-LOG_VIEW_CHARACTER_LIMIT) || 'Pane is empty.'} />
                {readable.kind === 'terminal' ? <Row gap={1}>
                  <Entry
                    value={input}
                    placeholder={'Send a line to this terminal'}
                    grow={true}
                    onChange={(event) => setInput(String(event.value ?? ''))}
                    onSubmit={() => { void sendLine(); }} />
                  <Button
                    label={'Send line'}
                    enabled={busy === '' && input.length > 0}
                    onInvoke={() => { void sendLine(); }} />
                </Row> : null}
              </CardContent>
            </Card> : null}
          </CardContent>
          <CardActions gap={1}>
            {busy === tab.id ? <Spinner /> : null}
            <Button
              label={`${tab.pinned ? 'Unpin' : 'Pin'} ${tab.title}`}
              enabled={busy === ''}
              onInvoke={() => { void pin(tab); }} />
            <Button
              label={`Focus ${tab.title}`}
              enabled={busy === '' && Boolean(tab.panes?.[0])}
              onInvoke={() => { void focus(tab); }} />
          </CardActions>
        </Card>)}
        <Omitted count={view.omitted} />
      </ResourceState>
    </Page>
  );
}

function Page({ title: label, subtitle, children }: { title: string; subtitle: string; children?: React.ReactNode }) { return (
  <Scroll grow={true} height={'fill'}>
    <Column pad={4} gap={2}>
      <Heading label={label} scale={'title'} />
      <Text label={subtitle} color={'text-dim'} wrap={true} />
      {children}
    </Column>
  </Scroll>
); }
function Toolbar({ loading, onRefresh }: { loading: boolean; onRefresh: () => void | Promise<void> }) { return (
  <Row gap={1} align={'center'}>
    {loading ? <Spinner /> : null}
    <Button label={'Refresh'} enabled={!loading} onInvoke={onRefresh} />
  </Row>
); }
function ErrorText({ error }: { error: unknown }) { return error ? <Text label={boundedMessage(error)} color={'danger'} wrap={true} /> : null; }
function InventoryEmpty<T>({ resource, records, label, detail }: {
  resource: Pick<Resource<T>, 'loading' | 'error'>; records: T[]; label: string; detail: string;
}) {
  return !resource.loading && !resource.error && records.length === 0
    ? <EmptyState label={label} detail={detail} />
    : null;
}
function Omitted({ count }: { count: number }) { return count > 0 ? <Text
  label={`${count} more records omitted to keep this view bounded.`}
  color={'text-dim'} /> : null; }

function title(value: string): string { return value.charAt(0).toUpperCase() + value.slice(1); }
function stateTone(state: string): 'positive' | 'warning' | 'neutral' { return state === 'running' ? 'positive' : state === 'paused' ? 'warning' : 'neutral'; }

function useResource<T>(loader: () => Promise<T[]>, initial?: T[]): Resource<T> {
  const [data, setData] = useState<T[] | undefined>(initial);
  const [loading, setLoading] = useState(initial === undefined);
  const [error, setError] = useState<unknown>(null);
  const revision = useRef(0);
  const reload = useCallback(async () => {
    const requested = ++revision.current;
    setLoading(true);
    try {
      const value = await loader();
      if (requested !== revision.current) return;
      setData(value); setError(null);
    } catch (cause) {
      if (requested === revision.current) setError(cause);
    } finally {
      if (requested === revision.current) setLoading(false);
    }
  }, [loader]);
  const replace = useCallback((value: T[]) => { revision.current += 1; setData(value); setError(null); setLoading(false); }, []);
  useEffect(() => {
    if (initial === undefined) void reload();
    return () => { revision.current += 1; };
  }, [initial, reload]);
  return { data, loading, error, reload, replace };
}
