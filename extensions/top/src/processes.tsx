import React from 'react';
import {
  Badge, Button, Card, CardContent, CardHeader, Column, Heading, ResourceState, Row, Scroll, Spinner, Text,
  type ContainerSummary, type ProcessList, type WorkspaceApi,
} from '@husklet/react';
import { bounded, boundedMessage, processRows, shortId } from './model.js';
import type { Resource } from './overview.js';

const SAMPLING_CONCURRENCY = 8;
type Snapshot = { container: ContainerSummary; rows: ProcessList; error: null };
type Failure = { container: ContainerSummary; rows: null; error: unknown };
type Group = Snapshot | Failure;

export function Processes({ api, resource }: { api: WorkspaceApi; resource: Resource<ContainerSummary> }) {
  const [snapshots, setSnapshots] = React.useState<Snapshot[]>([]);
  const [failures, setFailures] = React.useState<Failure[]>([]);
  const [loading, setLoading] = React.useState(true);
  const [error, setError] = React.useState<unknown>(null);
  const loadRevision = React.useRef(0);
  const load = React.useCallback(async () => {
    const revision = ++loadRevision.current;
    setLoading(true);
    try {
      const containers = resource.data ?? [];
      const groups: Array<Group | undefined> = new Array(containers.length);
      let cursor = 0;
      const worker = async () => {
        while (cursor < containers.length) {
          const index = cursor; cursor += 1;
          const container = containers[index];
          try { groups[index] = { container, rows: await api.containers.processes(container.id), error: null }; }
          catch (cause) { groups[index] = { container, rows: null, error: cause }; }
        }
      };
      await Promise.all(Array.from({ length: Math.min(SAMPLING_CONCURRENCY, containers.length) }, worker));
      if (revision !== loadRevision.current) return;
      const complete = groups.filter((group): group is Group => group !== undefined);
      const available = complete.filter((group): group is Snapshot => group.rows !== null);
      const unavailable = complete.filter((group): group is Failure => group.rows === null);
      setSnapshots(available); setFailures(unavailable);
      setError(available.length === 0 && unavailable.length > 0 ? unavailable[0].error : null);
    } finally {
      if (revision === loadRevision.current) setLoading(false);
    }
  }, [api, resource.data]);
  React.useEffect(() => {
    void load();
    return () => { loadRevision.current += 1; };
  }, [load]);
  const processes = snapshots.flatMap(({ container, rows }) => processRows(rows, container.name || shortId(container.id)));
  const observed = Math.max(0, ...snapshots.map(({ rows }) => Number(rows.observed_at_ms) || 0));
  const completeNamespace = snapshots.length > 0 && snapshots.every(({ rows }) => rows.scope === 'namespace');
  const view = bounded(processes);
  const failure = error ?? resource.error;
  const state: 'loading' | 'error' | 'empty' | 'ready' = loading || resource.loading
    ? 'loading' : failure ? 'error' : view.records.length === 0 ? 'empty' : 'ready';
  return <Page title="Processes" subtitle="A bounded snapshot across all visible containers.">
    <Toolbar loading={state === 'loading'} onRefresh={load} />
    <ResourceState
      state={state}
      loadingLabel="Reading processes…"
      emptyLabel="No running processes"
      emptyDetail="Start a container to see its process snapshot here."
      error={boundedMessage(failure)}
      retryLabel="Retry processes"
      onRetry={resource.error ? resource.reload : load}>
      <Text
        label={completeNamespace
          ? 'Full container namespace snapshots; PIDs identify only this observation and may be reused.'
          : 'Initial processes only; PIDs identify this snapshot and may be reused.'}
        color="text-dim" wrap />
      {observed > 0 ? <Text label={`Observed ${new Date(observed).toISOString()}`} color="text-dim" /> : null}
      {view.records.map((process, index) => {
        const pid = process.cells.PID ?? process.cells.Pid ?? process.cells.pid ?? '—';
        const command = process.cells.CMD ?? process.cells.Command ?? process.cells.COMMAND ?? process.values.at(-1) ?? 'Process';
        const detail = Object.entries(process.cells)
          .filter(([key]) => !['PID', 'Pid', 'pid', 'CMD', 'Command', 'COMMAND'].includes(key))
          .map(([key, value]) => `${key} ${value}`).join(' · ');
        return <Card key={`${process.container}:${pid}:${index}`} variant="outline">
          <CardHeader label={command} detail={process.container} />
          <CardContent><Row gap={2}><Badge label={`PID ${pid}`} /><Text label={detail} color="text-dim" /></Row></CardContent>
        </Card>;
      })}
      <Omitted count={view.omitted} />
      {snapshots.some(({ rows }) => rows.truncated)
        ? <Text label="The host process snapshot was truncated at its safety limit." color="warning" wrap /> : null}
    </ResourceState>
    {snapshots.length > 0 && failures.length > 0 ? <Column gap={1}>
      <Text
        label={`${failures.length} container process snapshot${failures.length === 1 ? '' : 's'} unavailable; available containers remain visible.`}
        color="warning" wrap />
      {failures.slice(0, 8).map(({ container, error: cause }) => <Text
        key={container.id}
        label={`${container.name || shortId(container.id)}: ${boundedMessage(cause, 256)}`}
        color="text-dim" wrap />)}
      {failures.length > 8 ? <Text label={`${failures.length - 8} more failures omitted.`} color="text-dim" /> : null}
    </Column> : null}
  </Page>;
}

function Page({ title, subtitle, children }: { title: string; subtitle: string; children: React.ReactNode }) {
  return <Scroll grow height="fill"><Column pad={4} gap={2}>
    <Heading label={title} scale="title" /><Text label={subtitle} color="text-dim" wrap />{children}
  </Column></Scroll>;
}
function Toolbar({ loading, onRefresh }: { loading: boolean; onRefresh: () => void | Promise<void> }) {
  return <Row gap={1} align="center">{loading ? <Spinner /> : null}<Button label="Refresh" enabled={!loading} onInvoke={onRefresh} /></Row>;
}
function Omitted({ count }: { count: number }) { return count > 0 ? <Text label={`${count} more records omitted to keep this view bounded.`} color="text-dim" /> : null; }
