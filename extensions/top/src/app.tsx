import React from 'react';
import {
  Badge, Button, Card, CardActions, CardContent, CardHeader, Column, Entry,
  ConfirmAction, EmptyState, Heading, KeyValueTable, List, ListItemButton, LogView, ResourceState, Row, Scroll, Separator, Spinner, Text,
  type ContainerSummary, type ExecutionSummary, type HostEvent, type ImageSummary, type NetworkSummary,
  type TabSummary, type VolumeSummary, type WorkspaceApi,
} from '@husklet/react';
import { ContainerDetailsSource, ExecutionDetailsSource, ImageDetailsSource, NetworkDetailsSource, VolumeDetailsSource, bounded, boundedMessage, bytes, endpointAliases, immutableContainerId, processRows, resourceReference, shortId } from './model.js';
import { ContainerDetail, type Inspection, type LifecycleAction } from './container-detail.js';
import { ContainerRename } from './container-rename.js';
import { ContainerCreate } from './container-create.js';
import { Navigation, Overview, SECTIONS, type Resource, type Section } from './overview.js';
import { Terminals } from './terminals.js';
import { Processes } from './processes.js';
import { Executions } from './executions.js';
import { Images } from './images.js';
import { Volumes } from './volumes.js';
import { Networks } from './networks.js';

export { Overview, SECTIONS } from './overview.js';
export { Terminals } from './terminals.js';
export { Processes } from './processes.js';
export { Executions } from './executions.js';
export { Images } from './images.js';
export { Volumes } from './volumes.js';
export { Networks } from './networks.js';
export { ContainerRename } from './container-rename.js';
export { ContainerCreate } from './container-create.js';
export { ContainerDetail } from './container-detail.js';

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
    let stop: (() => void) | null = null;
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
    if ('snapshot' in event && event.snapshot === 'containers') void containers.reload();
    if ('snapshot' in event && event.snapshot === 'images') void images.reload();
    if ('snapshot' in event && event.snapshot === 'volumes') void volumes.reload();
    if ('snapshot' in event && event.snapshot === 'networks') void networks.reload();
    if ('snapshot' in event && event.snapshot === 'terminal') void terminals.reload();
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
    onOpenExecution={async (id: string) => {
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

type ContainersProps = {
  api: WorkspaceApi;
  resource: Resource<ContainerSummary>;
  containerDetails?: ContainerDetailsSource;
  onOpenExecution?: (id: string) => void | Promise<void>;
};
export function Containers({ api, resource, containerDetails, onOpenExecution }: ContainersProps) {
  const localDetails = useMemo(() => new ContainerDetailsSource(), []);
  const detailsSource = containerDetails ?? localDetails;
  const [selected, setSelected] = useState<string | null>(null);
  const [busy, setBusy] = useState('');
  const [inspection, setInspection] = useState<Inspection>({ id: '', state: 'idle', count: 0, detail: null, error: null });
  const inspectionRevision = useRef(0);
  const inventoryRevision = useRef(resource.data);
  const currentContainers = useRef(new Map<string, string>());
  currentContainers.current = new Map((resource.data ?? []).map((container) => [container.id, container.state]));
  const act: LifecycleAction = async (verb, id, signal) => {
    setBusy(`${verb}:${id}`);
    try {
      if (verb === 'kill') await api.containers.kill(id, signal ?? 'SIGKILL');
      else await api.containers[verb](id);
      await resource.reload();
    } finally { setBusy(''); }
  };
  const inspect = async (item: ContainerSummary) => {
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
  const toggleDetails = (item: ContainerSummary) => {
    if (selected === item.id && inspection.state !== 'error') {
      setSelected(null);
      return;
    }
    void inspect(item);
  };
  const remove = async (item: ContainerSummary) => {
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
      <ContainerCreate
        api={api}
        blocked={busy !== ''}
        onBusyChange={(creating) => setBusy(creating ? 'create' : '')}
        reload={resource.reload} />
      <Toolbar loading={resource.loading} onRefresh={resource.reload} />
      <ResourceState
        state={state}
        loadingLabel={'Reading containers…'}
        emptyLabel={'No containers'}
        emptyDetail={'Create a container through an agent or extension, then refresh this page.'}
        error={boundedMessage(resource.error)}
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

function containerActions(
  item: ContainerSummary,
  busy: string,
  act: LifecycleAction,
  remove: (item: ContainerSummary) => void | Promise<void>,
): React.ReactNode[] {
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
