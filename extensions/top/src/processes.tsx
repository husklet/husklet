import React from 'react';
import {
  Button,
  Column,
  DataTable,
  Entry,
  IconButton,
  RecoveryState,
  Heading,
  ResourceState,
  Row,
  Scroll,
  Spinner,
  Text,
  type ContainerSummary,
  type ProcessList,
  type SortReport,
  type WorkspaceApi,
} from '@husklet/react';
import {
  PROCESS_TABLE_SOURCE,
  ProcessTableSource,
  bounded,
  boundedMessage,
  processRows,
  processTableSchema,
  shortId,
} from './model.js';
import type { Resource } from './overview.js';

const SAMPLING_CONCURRENCY = 8;
const OBSERVED_AT = new Intl.DateTimeFormat('en-US', {
  timeZone: 'UTC',
  month: 'short',
  day: 'numeric',
  year: 'numeric',
  hour: '2-digit',
  minute: '2-digit',
  hourCycle: 'h23',
});
type Snapshot = { container: ContainerSummary; rows: ProcessList; error: null };
type Failure = { container: ContainerSummary; rows: null; error: unknown };
type Group = Snapshot | Failure;

export function Processes({
  api,
  resource,
  processTable,
  onOpenContainers,
}: {
  api: WorkspaceApi;
  resource: Resource<ContainerSummary>;
  processTable?: ProcessTableSource;
  onOpenContainers?: () => void;
}) {
  const localTable = React.useMemo(() => new ProcessTableSource(), []);
  const table = processTable ?? localTable;
  const [snapshots, setSnapshots] = React.useState<Snapshot[]>([]);
  const [failures, setFailures] = React.useState<Failure[]>([]);
  const [loading, setLoading] = React.useState(true);
  const [error, setError] = React.useState<unknown>(null);
  const [filter, setFilter] = React.useState('');
  const [sort, setSort] = React.useState({ column: 'container', descending: false });
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
          const index = cursor;
          cursor += 1;
          const container = containers[index];
          try {
            groups[index] = {
              container,
              rows: await api.containers.processes(container.id),
              error: null,
            };
          } catch (cause) {
            groups[index] = { container, rows: null, error: cause };
          }
        }
      };
      await Promise.all(
        Array.from({ length: Math.min(SAMPLING_CONCURRENCY, containers.length) }, worker),
      );
      if (revision !== loadRevision.current) return;
      const complete = groups.filter((group): group is Group => group !== undefined);
      const available = complete.filter((group): group is Snapshot => group.rows !== null);
      const unavailable = complete.filter((group): group is Failure => group.rows === null);
      setSnapshots(available);
      setFailures(unavailable);
      setError(available.length === 0 && unavailable.length > 0 ? unavailable[0].error : null);
    } finally {
      if (revision === loadRevision.current) setLoading(false);
    }
  }, [api, resource.data]);
  React.useEffect(() => {
    void load();
    return () => {
      loadRevision.current += 1;
    };
  }, [load]);
  const processes = React.useMemo(
    () =>
      snapshots.flatMap(({ container, rows }) =>
        processRows(rows, container.name || shortId(container.id)),
      ),
    [snapshots],
  );
  const observed = Math.max(0, ...snapshots.map(({ rows }) => Number(rows.observed_at_ms) || 0));
  const completeNamespace =
    snapshots.length > 0 && snapshots.every(({ rows }) => rows.scope === 'namespace');
  const view = React.useMemo(() => bounded(processes), [processes]);
  const schema = React.useMemo(() => processTableSchema(view.records), [view.records]);
  React.useEffect(() => {
    void table.replace(view.records, schema, filter, sort.column, sort.descending);
  }, [filter, schema, sort.column, sort.descending, table, view.records]);
  const failure = error ?? resource.error;
  const state: 'loading' | 'error' | 'empty' | 'ready' =
    loading || resource.loading
      ? 'loading'
      : failure
        ? 'error'
        : view.records.length === 0
          ? 'empty'
          : 'ready';
  const partialFailureSummary = `${failures.length} container process snapshot${failures.length === 1 ? '' : 's'} unavailable; available containers remain visible.`;
  const partialFailureDiagnostic = [
    ...failures
      .slice(0, 8)
      .map(
        ({ container, error: cause }) =>
          `${container.name || shortId(container.id)}: ${boundedMessage(cause, 256)}`,
      ),
    ...(failures.length > 8 ? [`${failures.length - 8} more failures omitted.`] : []),
  ].join('\n');
  return (
    <Page
      title="Processes"
      subtitle="Live process snapshots across visible containers; nothing here is a durable command record."
      action={<Toolbar loading={state === 'loading'} onRefresh={load} />}
    >
      <ResourceState
        state={state}
        loadingLabel="Reading processes…"
        emptyLabel="No running processes"
        emptyDetail="Start a container to see its process snapshot here."
        error={boundedMessage(failure)}
        retryLabel="Retry processes"
        onRetry={resource.error ? resource.reload : load}
      >
        {snapshots.length > 0 && failures.length > 0 ? (
          <RecoveryState
            summary={partialFailureSummary}
            tone="warning"
            error={partialFailureDiagnostic}
          />
        ) : null}
        <Text
          label={
            completeNamespace
              ? 'Full container namespace snapshots; PIDs identify only this observation and may be reused.'
              : 'Initial processes only; PIDs identify this snapshot and may be reused.'
          }
          color="text-dim"
          wrap
        />
        {observed > 0 ? (
          <Text label={`Observed ${OBSERVED_AT.format(observed)} UTC`} color="text-dim" />
        ) : null}
        <Entry
          value={filter}
          placeholder="Filter container, user, PID, or command"
          width={{ minimum: { chars: 18 }, maximum: { chars: 52 } }}
          onChange={(event) => setFilter(String(event.value ?? '').slice(0, 256))}
        />
        <DataTable
          source={PROCESS_TABLE_SOURCE}
          schema={schema}
          width="fill"
          height={{ minimum: { step: 20 }, maximum: { step: 80 } }}
          onSort={(event: SortReport) => {
            if (table.accepts(event)) {
              setSort({ column: event.column, descending: Boolean(event.descending) });
            }
          }}
        />
        {filter.trim() ? (
          <Text label="Filter applies to the bounded snapshot shown here." color="text-dim" />
        ) : null}
        <Omitted count={view.omitted} />
        {snapshots.some(({ rows }) => rows.truncated) ? (
          <Text
            label="The host process snapshot was truncated at its safety limit."
            color="warning"
            wrap
          />
        ) : null}
      </ResourceState>
      {state === 'empty' && onOpenContainers ? (
        <Row width="fill" justify="center">
          <Button label="Open containers" onInvoke={onOpenContainers} />
        </Row>
      ) : null}
    </Page>
  );
}

function Page({
  title,
  subtitle,
  action,
  children,
}: {
  title: string;
  subtitle: string;
  action?: React.ReactNode;
  children: React.ReactNode;
}) {
  return (
    <Scroll grow width="fill" height="fill">
      <Column width="fill" pad={4} gap={3}>
        <Row gap={2} align="center" justify="start" wrap>
          <Heading label={title} scale="display" />
          {action}
        </Row>
        <Text label={subtitle} color="text-dim" wrap />
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
    <Row gap={1} align="center">
      {loading ? <Spinner /> : null}
      <IconButton
        label="Refresh"
        tooltip="Refresh processes"
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
    <Text label={`${count} more records omitted to keep this view bounded.`} color="text-dim" />
  ) : null;
}
