import React from 'react';
import {
  Badge, Button, Card, CardActions, CardContent, CardHeader, Column, ConfirmAction, Heading,
  ResourceState, Row, Scroll, Spinner, Text,
  type ContainerSummary, type WorkspaceApi,
} from '@husklet/react';
import { ContainerDetailsSource, bounded, boundedMessage, shortId } from './model.js';
import { ContainerCreate } from './container-create.js';
import { ContainerDetail, type Inspection, type LifecycleAction } from './container-detail.js';
import { ContainerRename } from './container-rename.js';
import type { Resource } from './overview.js';

const { useEffect, useMemo, useRef, useState } = React;

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
function Omitted({ count }: { count: number }) { return count > 0 ? <Text
  label={`${count} more records omitted to keep this view bounded.`}
  color={'text-dim'} /> : null; }

function stateTone(state: string): 'positive' | 'warning' | 'neutral' { return state === 'running' ? 'positive' : state === 'paused' ? 'warning' : 'neutral'; }
