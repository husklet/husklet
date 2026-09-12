import React from 'react';
import {
  Badge,
  Button,
  Card,
  Column,
  ConfirmAction,
  Expander,
  Heading,
  InlineButton,
  IconButton,
  ResourceState,
  Row,
  Scroll,
  Spinner,
  Text,
  type ContainerSummary,
  type WorkspaceApi,
} from '@husklet/react';
import { ContainerDetailsSource, bounded, boundedMessage, shortId } from './model.js';
import { ContainerCreate } from './container-create.js';
import {
  ContainerDetail,
  type Inspection,
  type LifecycleAction,
  type LifecycleVerb,
} from './container-detail.js';
import { ContainerRename } from './container-rename.js';
import type { Resource } from './overview.js';
import { ResourceSummary } from './resource-summary.js';

const { useEffect, useMemo, useRef, useState } = React;

type ContainersProps = {
  api: WorkspaceApi;
  resource: Resource<ContainerSummary>;
  containerDetails?: ContainerDetailsSource;
  onOpenExecution?: (id: string) => void | Promise<void>;
  onOpenExtensions: () => void;
};
export function Containers({
  api,
  resource,
  containerDetails,
  onOpenExecution,
  onOpenExtensions,
}: ContainersProps) {
  const localDetails = useMemo(() => new ContainerDetailsSource(), []);
  const detailsSource = containerDetails ?? localDetails;
  const [selected, setSelected] = useState<string | null>(null);
  const [busy, setBusy] = useState('');
  const [notice, setNotice] = useState<{
    tone: 'positive' | 'warning' | 'danger';
    label: string;
  } | null>(null);
  const [inspection, setInspection] = useState<Inspection>({
    id: '',
    state: 'idle',
    count: 0,
    detail: null,
    error: null,
  });
  const inspectionRevision = useRef(0);
  const inventoryRevision = useRef(resource.data);
  const currentContainers = useRef(new Map<string, string>());
  currentContainers.current = new Map(
    (resource.data ?? []).map((container) => [container.id, container.state]),
  );
  const act: LifecycleAction = async (verb, id, signal, generation) => {
    setBusy(`${verb}:${id}`);
    setNotice(null);
    try {
      let verified: boolean | null = null;
      if (generation === undefined)
        throw new Error(
          `Container ${id} has no observable generation; refresh before changing it.`,
        );
      if (verb === 'start') verified = (await api.containers.startAndWait(id, generation)).changed;
      else if (verb === 'stop')
        verified = (await api.containers.stopAndWait(id, generation)).changed;
      else if (verb === 'restart') {
        if (generation === undefined)
          throw new Error(
            `Container ${id} has no observable generation; refresh before restarting it.`,
          );
        verified = (await api.containers.restartAndWait(id, generation)).changed;
      } else if (verb === 'kill') await api.containers.kill(id, generation, signal ?? 'SIGKILL');
      else await api.containers[verb](id, generation);
      await resource.reload();
      setNotice(
        verified === false
          ? {
              tone: 'warning',
              label: `${lifecycleLabel(verb)} was sent, but the requested transition was not observed before the deadline.`,
            }
          : {
              tone: 'positive',
              label:
                verified === true
                  ? `${lifecycleLabel(verb)} completed and was verified.`
                  : `${lifecycleLabel(verb)} was accepted; refreshed current container state.`,
            },
      );
    } catch (cause) {
      setNotice({ tone: 'danger', label: boundedMessage(cause) });
    } finally {
      setBusy('');
    }
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
      if (revision === inspectionRevision.current)
        setInspection({ id: item.id, state: 'error', count: 0, detail: null, error: cause });
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
    const current = currentContainers.current.get(item.id);
    if (!removable(current) || !removable(item.state)) {
      throw new Error(
        `Container ${item.id} changed or is no longer created or exited; refresh and confirm again.`,
      );
    }
    setBusy(`remove:${item.id}`);
    setNotice(null);
    try {
      const removed = await api.containers.removeAndWait(item.id, item.generation);
      inspectionRevision.current += 1;
      setSelected(null);
      setInspection({ id: '', state: 'idle', count: 0, detail: null, error: null });
      await detailsSource.replace(null);
      await resource.reload();
      setNotice(
        removed.changed
          ? { tone: 'positive', label: 'Container removal completed and its absence was verified.' }
          : {
              tone: 'warning',
              label:
                'Removal was sent, but container absence was not observed before the deadline.',
            },
      );
    } catch (cause) {
      setNotice({ tone: 'danger', label: boundedMessage(cause) });
    } finally {
      setBusy('');
    }
  };
  const view = bounded(resource.data);
  const state = resource.loading
    ? 'loading'
    : resource.error
      ? 'error'
      : view.records.length === 0
        ? 'empty'
        : 'ready';
  const createControl = (
    <ContainerCreate
      api={api}
      blocked={busy !== ''}
      label={state === 'empty' ? 'Create first container' : undefined}
      prominent={state === 'empty'}
      onBusyChange={(creating) => setBusy(creating ? 'create' : '')}
      reload={resource.reload}
    />
  );
  return (
    <Page
      title={'Containers'}
      subtitle={'Create and manage containers; inspect lifecycle, logs, and execution.'}
      action={<Toolbar loading={resource.loading} onRefresh={resource.reload} />}
    >
      {state === 'empty' ? null : createControl}
      {notice ? <Text label={notice.label} color={notice.tone} wrap /> : null}
      {state === 'empty' ? (
        <Column gap={1} align="center" pad={{ top: 3 }}>
          <Heading label="No containers" scale="caption" />
          <Text label="Create a container to start a service or open a shell." color="text-dim" />
          {createControl}
        </Column>
      ) : (
        <ResourceState
          state={state}
          loadingLabel={'Reading containers…'}
          error={boundedMessage(resource.error)}
          retryLabel={'Retry containers'}
          onRetry={resource.reload}
        >
          {view.records.map((item) => (
            <Card key={item.id} variant={selected === item.id ? 'filled' : 'outline'} width="fill">
              <ResourceSummary
                label={item.name || shortId(item.id)}
                detail={item.image}
                status={
                  <>
                    <Badge label={item.state} tone={stateTone(item.state)} />
                    <Text label={`ID ${shortId(item.id)}`} color="text-dim" />
                  </>
                }
                actions={
                  <>
                    {selected === item.id &&
                    inspection.state === 'error' &&
                    isAuthorityDenial(inspection.error) ? null : (
                      <InlineButton
                        label={
                          selected === item.id
                            ? inspection.state === 'loading'
                              ? 'Reading details…'
                              : inspection.state === 'error'
                                ? 'Retry details'
                                : 'Hide details'
                            : 'Details'
                        }
                        variant="filled"
                        tone="accent"
                        enabled={busy === ''}
                        onInvoke={() => toggleDetails(item)}
                      />
                    )}
                    {startable(item.state) ? (
                      <InlineButton
                        label="Start"
                        variant="outline"
                        enabled={busy === ''}
                        onInvoke={() => act('start', item.id, undefined, item.generation)}
                      />
                    ) : null}
                  </>
                }
                overflow={
                  <ContainerActions
                    api={api}
                    item={item}
                    busy={busy}
                    act={act}
                    remove={remove}
                    reload={resource.reload}
                  />
                }
              />
              {selected === item.id ? (
                <ContainerDetail
                  api={api}
                  container={item}
                  act={act}
                  inspection={inspection}
                  onRetry={() => inspect(item)}
                  onOpenExecution={onOpenExecution}
                  onOpenExtensions={onOpenExtensions}
                />
              ) : null}
            </Card>
          ))}
          <Omitted count={view.omitted} />
        </ResourceState>
      )}
    </Page>
  );
}

function ContainerActions({
  api,
  item,
  busy,
  act,
  remove,
  reload,
}: {
  api: WorkspaceApi;
  item: ContainerSummary;
  busy: string;
  act: LifecycleAction;
  remove: (item: ContainerSummary) => void | Promise<void>;
  reload: () => void | Promise<void>;
}) {
  const blocked = busy !== '';
  const running = item.state === 'running';
  const active = running || item.state === 'paused';
  return (
    <Expander
      label="More actions"
      expanded={false}
      variant="outline"
      width="content"
      align="start"
      justify="center"
      tooltip="Rename, restart, pause, stop, or remove this container"
    >
      <Column gap={1} align="start" width="fill">
        <ContainerRename api={api} container={item} reload={reload} blocked={blocked} />
        <Row gap={1} wrap align="center">
          {active ? (
            <Button
              label="Restart"
              variant="ghost"
              size="small"
              enabled={!blocked}
              onInvoke={() => act('restart', item.id, undefined, item.generation)}
            />
          ) : null}
          {active ? (
            <Button
              label={item.state === 'paused' ? 'Resume' : 'Pause'}
              variant="ghost"
              size="small"
              enabled={!blocked}
              onInvoke={() =>
                act(
                  item.state === 'paused' ? 'unpause' : 'pause',
                  item.id,
                  undefined,
                  item.generation,
                )
              }
            />
          ) : null}
          {active || item.state === 'restarting' ? (
            <ConfirmAction
              label="Stop"
              confirmLabel="Confirm stop"
              pendingLabel="Confirm stop"
              authorityKey={`container:${item.id}:stop`}
              question={`Stop ${item.name || shortId(item.id)} with immutable ID ${item.id}?`}
              enabled={!blocked}
              size="small"
              onConfirm={() => act('stop', item.id, undefined, item.generation)}
            />
          ) : null}
          {removable(item.state) ? (
            <ConfirmAction
              label="Remove"
              confirmLabel="Confirm remove"
              pendingLabel="Confirm remove"
              authorityKey={`container:${item.id}:remove`}
              question={`Remove inactive container ${item.name || shortId(item.id)} with immutable ID ${item.id}?`}
              enabled={!blocked}
              size="small"
              onConfirm={() => remove(item)}
            />
          ) : null}
        </Row>
      </Column>
    </Expander>
  );
}

function isAuthorityDenial(error: unknown): boolean {
  if (!error || typeof error !== 'object') return false;
  const failure = error as { kind?: unknown; message?: unknown };
  return (
    failure.kind === 'denied' ||
    (typeof failure.message === 'string' && failure.message.includes('consented resource scope'))
  );
}

function Page({
  title: label,
  subtitle,
  action,
  children,
}: {
  title: string;
  subtitle: string;
  action?: React.ReactNode;
  children?: React.ReactNode;
}) {
  return (
    <Scroll grow={true} height={'fill'}>
      <Column width="fill" pad={4} gap={3}>
        <Row gap={2} align="center" justify="start" wrap>
          <Heading label={label} scale={'display'} />
          {action}
        </Row>
        <Text label={subtitle} color={'text-dim'} wrap={true} />
        {children}
      </Column>
    </Scroll>
  );
}
function Toolbar({
  loading,
  onRefresh,
}: {
  loading: boolean;
  onRefresh: () => void | Promise<void>;
}) {
  return (
    <Row gap={1} align={'center'}>
      {loading ? <Spinner /> : null}
      <IconButton
        label="Refresh"
        tooltip="Refresh containers"
        icon="view-refresh-symbolic"
        size="small"
        variant="ghost"
        enabled={!loading}
        onInvoke={onRefresh}
      />
    </Row>
  );
}
function Omitted({ count }: { count: number }) {
  return count > 0 ? (
    <Text label={`${count} more records omitted to keep this view bounded.`} color={'text-dim'} />
  ) : null;
}

function stateTone(state: string): 'positive' | 'warning' | 'neutral' {
  return state === 'running' ? 'positive' : state === 'paused' ? 'warning' : 'neutral';
}
function removable(state: string | undefined): boolean {
  return state === 'created' || state === 'exited';
}
function startable(state: string | undefined): boolean {
  return state === 'created' || state === 'exited';
}
function lifecycleLabel(verb: LifecycleVerb): string {
  return verb === 'unpause' ? 'Resume' : verb.charAt(0).toUpperCase() + verb.slice(1);
}
