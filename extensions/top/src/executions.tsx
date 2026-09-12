import React from 'react';
import {
  Badge,
  Button,
  Card,
  CardContent,
  Column,
  ConfirmAction,
  Expander,
  Heading,
  IconButton,
  InlineMessage,
  KeyValueTable,
  LogView,
  ResourceState,
  Row,
  Scroll,
  Spinner,
  Text,
  LOG_VIEW_CHARACTER_LIMIT,
  type ExecutionSummary,
  type WorkspaceApi,
} from '@husklet/react';
import {
  EXECUTION_DETAIL_SOURCE,
  ExecutionDetailsSource,
  bounded,
  boundedMessage,
  logText,
  shortId,
} from './model.js';
import type { Resource } from './overview.js';
import { ResourceSummary } from './resource-summary.js';

const DETAIL_SCHEMA = [
  { key: 'property', title: 'Property', width: { chars: 20 } },
  { key: 'value', title: 'Value', width: 'fill' as const },
];

type Inspection = {
  state: 'idle' | 'loading' | 'ready' | 'error';
  count: number;
  detail: ExecutionSummary | null;
  error: unknown;
};
type Output = {
  revision: number;
  stdout: string;
  stderr: string;
  truncated: boolean;
  stdoutTruncated?: boolean;
  stderrTruncated?: boolean;
  eof?: boolean;
};

export interface ExecutionsProps {
  api: WorkspaceApi;
  resource: Resource<ExecutionSummary>;
  executionDetails?: ExecutionDetailsSource;
  truncated?: boolean;
  requestedExecution?: string;
  onOpenContainers?: () => void;
}

export function Executions({
  api,
  resource,
  executionDetails,
  truncated = false,
  requestedExecution = '',
  onOpenContainers,
}: ExecutionsProps) {
  const localDetails = React.useMemo(() => new ExecutionDetailsSource(), []);
  const detailsSource = executionDetails ?? localDetails;
  const [selected, setSelected] = React.useState('');
  const [inspection, setInspection] = React.useState<Inspection>({
    state: 'idle',
    count: 0,
    detail: null,
    error: null,
  });
  const [output, setOutput] = React.useState<Output | null>(null);
  const [busy, setBusy] = React.useState('');
  const [notice, setNotice] = React.useState<{
    tone: 'positive' | 'warning' | 'danger';
    label: string;
  } | null>(null);
  const [inventoryVersion, setInventoryVersion] = React.useState(0);
  const lifecycleRevision = React.useRef(0);
  const inventoryRevision = React.useRef(resource.data);

  const inspect = React.useCallback(
    async (id: string) => {
      const revision = ++lifecycleRevision.current;
      setSelected(id);
      setInspection({ state: 'loading', count: 0, detail: null, error: null });
      setOutput(null);
      try {
        const detail = await api.containers.execution(id);
        if (revision !== lifecycleRevision.current) return;
        const count = await detailsSource.replace(detail);
        if (revision !== lifecycleRevision.current) return;
        setInspection({ state: 'ready', count, detail, error: null });
      } catch (error) {
        if (revision === lifecycleRevision.current)
          setInspection({ state: 'error', count: 0, detail: null, error });
      }
    },
    [api, detailsSource],
  );

  React.useEffect(() => {
    if (requestedExecution && selected !== requestedExecution) void inspect(requestedExecution);
  }, [inspect, requestedExecution, selected]);

  const logs = async (id: string) => {
    const revision = ++lifecycleRevision.current;
    setSelected(id);
    setInspection({ state: 'loading', count: 0, detail: null, error: null });
    setOutput(null);
    setBusy(`logs:${id}`);
    try {
      const detailRequest = api.containers.execution(id);
      const outputRequest = api.containers.executionLogs(id, { stdout: true, stderr: true });
      try {
        const detail = await detailRequest;
        if (revision !== lifecycleRevision.current) return;
        const count = await detailsSource.replace(detail);
        if (revision !== lifecycleRevision.current) return;
        setInspection({ state: 'ready', count, detail, error: null });
      } catch (cause) {
        if (revision !== lifecycleRevision.current) return;
        setInspection({ state: 'error', count: 0, detail: null, error: cause });
      }
      const value = await outputRequest;
      if (revision !== lifecycleRevision.current) return;
      setOutput((current) => ({
        revision: (current?.revision ?? 0) + 1,
        stdout: logText(value.stdout).slice(-LOG_VIEW_CHARACTER_LIMIT),
        stderr: logText(value.stderr).slice(-LOG_VIEW_CHARACTER_LIMIT),
        truncated: value.truncated,
        stdoutTruncated: value.stdout_truncated,
        stderrTruncated: value.stderr_truncated,
        eof: value.eof,
      }));
    } finally {
      if (revision === lifecycleRevision.current) setBusy('');
    }
  };
  const wait = async (id: string) => {
    const revision = lifecycleRevision.current;
    setBusy(`wait:${id}`);
    try {
      const detail = await api.containers.waitExecution(id, { timeoutMs: 5_000 });
      if (revision !== lifecycleRevision.current) return;
      await detailsSource.replace(detail);
      if (revision === lifecycleRevision.current) await resource.reload();
    } finally {
      if (revision === lifecycleRevision.current) setBusy('');
    }
  };
  const terminate = async (item: ExecutionSummary) => {
    setBusy(`terminate:${item.id}`);
    setNotice(null);
    try {
      const result = await api.containers.signalExecutionAndWait(
        item.id,
        'SIGTERM',
        {
          running: item.running,
          exit_code: item.exit_code,
          pid: item.pid,
        },
        { state: 'exited' },
      );
      await resource.reload();
      await inspect(item.id);
      setNotice(
        result.changed
          ? {
              tone: 'positive',
              label: `SIGTERM completed and execution ${shortId(item.id)} was observed exited.`,
            }
          : {
              tone: 'warning',
              label: `SIGTERM was sent, but execution ${shortId(item.id)} was not observed exited before the deadline.`,
            },
      );
    } catch (error) {
      setNotice({ tone: 'danger', label: boundedMessage(error) });
    } finally {
      setBusy('');
    }
  };
  const remove = async (item: ExecutionSummary) => {
    setBusy(`remove:${item.id}`);
    setNotice(null);
    try {
      const result = await api.containers.removeExecutionAndWait(item.id, {
        running: item.running,
        exit_code: item.exit_code,
        pid: item.pid,
      });
      setSelected('');
      setOutput(null);
      await resource.reload();
      setNotice(
        result.changed
          ? {
              tone: 'positive',
              label: `Execution ${shortId(item.id)} was removed and its absence was verified.`,
            }
          : {
              tone: 'warning',
              label: `Removal was sent, but execution ${shortId(item.id)} absence was not observed before the deadline.`,
            },
      );
    } catch (error) {
      setNotice({ tone: 'danger', label: boundedMessage(error) });
    } finally {
      setBusy('');
    }
  };

  React.useEffect(() => {
    if (inventoryRevision.current === resource.data) return;
    inventoryRevision.current = resource.data;
    lifecycleRevision.current += 1;
    setSelected('');
    setInspection({ state: 'idle', count: 0, detail: null, error: null });
    setOutput(null);
    setBusy('');
    setInventoryVersion((version) => version + 1);
  }, [resource.data]);

  const view = bounded(resource.data);
  const state: 'loading' | 'error' | 'empty' | 'ready' = resource.loading
    ? 'loading'
    : resource.error
      ? 'error'
      : view.records.length === 0
        ? 'empty'
        : 'ready';
  return (
    <Page
      title="Executions"
      subtitle="Durable command records created inside containers, with status and captured output."
      action={<Toolbar loading={resource.loading} onRefresh={resource.reload} />}
    >
      {notice ? <Text label={notice.label} color={notice.tone} wrap /> : null}
      <ResourceState
        state={state}
        loadingLabel="Reading executions…"
        emptyLabel="No executions"
        emptyDetail="Commands executed in containers will appear here."
        error={boundedMessage(resource.error)}
        retryLabel="Retry executions"
        onRetry={resource.reload}
      >
        {view.records.map((item) => (
          <Card
            key={`${inventoryVersion}:${item.id}`}
            variant={selected === item.id ? 'filled' : 'outline'}
            width="fill"
          >
            <ResourceSummary
              label={item.command?.join(' ') || shortId(item.id)}
              detail={`container ${shortId(item.container_id)}`}
              status={
                <Badge
                  label={item.running ? 'running' : `exited ${item.exit_code}`}
                  tone={item.running ? 'positive' : 'neutral'}
                />
              }
              actions={
                <>
                  <Button
                    label={selected === item.id ? 'Hide details' : 'Details'}
                    size="small"
                    variant="filled"
                    tone="accent"
                    enabled={!busy}
                    onInvoke={() =>
                      selected === item.id ? setSelected('') : void inspect(item.id)
                    }
                  />
                  <Button
                    label={busy === `logs:${item.id}` ? 'Loading logs…' : 'Load output'}
                    size="small"
                    enabled={!busy}
                    onInvoke={() => void logs(item.id)}
                  />
                  <Button
                    label={busy === `wait:${item.id}` ? 'Waiting…' : 'Wait up to 5s'}
                    size="small"
                    enabled={!busy && item.running}
                    onInvoke={() => void wait(item.id)}
                  />
                </>
              }
              overflow={
                <Expander
                  label="More actions"
                  variant="outline"
                  width="content"
                  align="start"
                  tooltip="Terminate this process or remove its completed execution record"
                >
                  <Column gap={1}>
                    <Text
                      label={
                        item.running
                          ? 'Terminate the running process.'
                          : 'Remove this completed execution record and its captured output.'
                      }
                      color="text-dim"
                      wrap
                    />
                    <Row gap={1} wrap>
                      <ConfirmAction
                        authorityKey={`execution:${item.id}:SIGTERM`}
                        label="Terminate"
                        confirmLabel="Confirm SIGTERM"
                        pendingLabel="Confirm SIGTERM"
                        question={`Send SIGTERM to execution ${item.id}?`}
                        enabled={!busy && item.running}
                        size="small"
                        onConfirm={() => terminate(item)}
                      />
                      <ConfirmAction
                        authorityKey={`execution:${item.id}:remove`}
                        label="Remove record"
                        confirmLabel="Confirm removal"
                        pendingLabel="Confirm removal"
                        question={`Remove execution record ${shortId(item.id)}?`}
                        enabled={!busy && !item.running}
                        size="small"
                        onConfirm={() => remove(item)}
                      />
                    </Row>
                  </Column>
                </Expander>
              }
            />
            <CardContent>
              {selected === item.id ? (
                <ExecutionDetail
                  inspection={inspection}
                  output={output}
                  outputLoading={busy === `logs:${item.id}`}
                  onRetry={() => inspect(item.id)}
                />
              ) : null}
            </CardContent>
          </Card>
        ))}
        <Omitted count={view.omitted} />
        {truncated ? (
          <Text
            label="The host execution catalogue was truncated at its safety limit."
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

function ExecutionDetail({
  inspection,
  output,
  outputLoading,
  onRetry,
}: {
  inspection: Inspection;
  output: Output | null;
  outputLoading: boolean;
  onRetry: () => void;
}) {
  const detailState =
    inspection.state === 'idle'
      ? 'loading'
      : inspection.state === 'ready' && inspection.count === 0
        ? 'empty'
        : inspection.state;
  if (inspection.state === 'error') {
    return (
      <Column gap={1}>
        <InlineMessage label={executionFailureSummary(inspection.error)} tone="danger" />
        <Row justify="start">
          <Button label="Retry details" variant="outline" tone="accent" onInvoke={onRetry} />
        </Row>
      </Column>
    );
  }
  return (
    <>
      <ResourceState
        state={detailState}
        loadingLabel="Reading execution details…"
        emptyLabel="No execution details"
        emptyDetail="The host returned no inspectable fields."
        error={boundedMessage(inspection.error)}
        retryLabel="Retry details"
        onRetry={onRetry}
      >
        <Column gap={2} width="fill">
          <ExecutionSummaryDetail value={inspection.detail} />
          <Expander label="Technical details" width="fill" align="start">
            <KeyValueTable source={EXECUTION_DETAIL_SOURCE} schema={DETAIL_SCHEMA} />
          </Expander>
        </Column>
      </ResourceState>
      {outputLoading ? (
        <Row gap={1} align="center">
          <Spinner />
          <Text label="Loading captured output…" />
        </Row>
      ) : null}
      {output ? (
        <Column gap={1}>
          <Heading label="Standard output" scale="caption" />
          <LogView
            key={`stdout-${output.revision}`}
            value={
              output.stdout ||
              (output.eof
                ? 'No stdout captured (EOF).'
                : 'No stdout captured yet; execution is still running.')
            }
            monospace
          />
          {output.stdoutTruncated ? (
            <Text label="Standard output was truncated to its configured bound." color="warning" />
          ) : null}
          <Heading label="Standard error" scale="caption" />
          <LogView
            key={`stderr-${output.revision}`}
            value={
              output.stderr ||
              (output.eof
                ? 'No stderr captured (EOF).'
                : 'No stderr captured yet; execution is still running.')
            }
            monospace
          />
          {output.stderrTruncated ? (
            <Text label="Standard error was truncated to its configured bound." color="warning" />
          ) : null}
          <Text
            label={
              output.eof
                ? 'Captured output is complete (EOF).'
                : 'Execution is still running; later output may appear.'
            }
            color="text-dim"
          />
          {output.truncated && !output.stdoutTruncated && !output.stderrTruncated ? (
            <Text label="Host output was truncated to its configured bound." color="warning" />
          ) : null}
        </Column>
      ) : null}
    </>
  );
}

function ExecutionSummaryDetail({ value }: { value: ExecutionSummary | null }) {
  if (!value) return null;
  return (
    <Column gap={1} width="fill">
      <Heading label="Execution summary" scale="caption" />
      {value.pid > 0 ? <Text label={`Process · ${value.pid}`} color="text-dim" /> : null}
      <Text label={`Command · ${value.command?.join(' ') || 'Unavailable'}`} wrap />
      <Text label={`User · ${value.user || 'Default user'}`} color="text-dim" wrap />
      <Text label={`Container · ${shortId(value.container_id)}`} color="text-dim" />
    </Column>
  );
}

function executionFailureSummary(error: unknown): string {
  const detail = boundedMessage(error, 256);
  if (/denied|permission|capabilit/i.test(detail)) {
    return 'Top does not have permission to inspect this execution. Review its execution access in Extensions.';
  }
  if (/not found|absent|no such|disappeared|moved/i.test(detail)) {
    return 'This execution is no longer available. Refresh executions to see the current records.';
  }
  if (/connection to husklet was interrupted/i.test(detail)) return detail;
  return detail
    ? `Execution details could not be loaded: ${detail}`
    : 'Execution details could not be loaded. Retry the request.';
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
        tooltip="Refresh executions"
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
