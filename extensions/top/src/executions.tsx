import React from 'react';
import {
  Badge, Button, Card, CardActions, CardContent, CardHeader, Column, ConfirmAction, Heading,
  KeyValueTable, LogView, ResourceState, Row, Scroll, Spinner, Text, LOG_VIEW_CHARACTER_LIMIT,
  type ExecutionSummary, type WorkspaceApi,
} from '@husklet/react';
import {
  EXECUTION_DETAIL_SOURCE, ExecutionDetailsSource, bounded, boundedMessage, logText, shortId,
} from './model.js';
import type { Resource } from './overview.js';

const DETAIL_SCHEMA = [
  { key: 'property', title: 'Property', width: { chars: 20 } },
  { key: 'value', title: 'Value', width: 'fill' as const },
];

type Inspection = {
  state: 'idle' | 'loading' | 'ready' | 'error';
  count: number;
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
}

export function Executions({
  api, resource, executionDetails, truncated = false, requestedExecution = '',
}: ExecutionsProps) {
  const localDetails = React.useMemo(() => new ExecutionDetailsSource(), []);
  const detailsSource = executionDetails ?? localDetails;
  const [selected, setSelected] = React.useState('');
  const [inspection, setInspection] = React.useState<Inspection>({ state: 'idle', count: 0, error: null });
  const [output, setOutput] = React.useState<Output | null>(null);
  const [busy, setBusy] = React.useState('');
  const [inventoryVersion, setInventoryVersion] = React.useState(0);
  const lifecycleRevision = React.useRef(0);
  const inventoryRevision = React.useRef(resource.data);

  const inspect = React.useCallback(async (id: string) => {
    const revision = ++lifecycleRevision.current;
    setSelected(id);
    setInspection({ state: 'loading', count: 0, error: null });
    setOutput(null);
    try {
      const detail = await api.containers.execution(id);
      if (revision !== lifecycleRevision.current) return;
      const count = await detailsSource.replace(detail);
      if (revision !== lifecycleRevision.current) return;
      setInspection({ state: 'ready', count, error: null });
    } catch (error) {
      if (revision === lifecycleRevision.current) setInspection({ state: 'error', count: 0, error });
    }
  }, [api, detailsSource]);

  React.useEffect(() => {
    if (requestedExecution && selected !== requestedExecution) void inspect(requestedExecution);
  }, [inspect, requestedExecution, selected]);

  const logs = async (id: string) => {
    const revision = lifecycleRevision.current;
    setBusy(`logs:${id}`);
    try {
      const value = await api.containers.executionLogs(id, { stdout: true, stderr: true });
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
  const terminate = async (id: string) => {
    await api.containers.signalExecution(id, 'SIGTERM');
    await resource.reload();
    await inspect(id);
  };
  const remove = async (id: string) => {
    await api.containers.removeExecution(id);
    setSelected('');
    setOutput(null);
    await resource.reload();
  };

  React.useEffect(() => {
    if (inventoryRevision.current === resource.data) return;
    inventoryRevision.current = resource.data;
    lifecycleRevision.current += 1;
    setSelected('');
    setInspection({ state: 'idle', count: 0, error: null });
    setOutput(null);
    setBusy('');
    setInventoryVersion((version) => version + 1);
  }, [resource.data]);

  const view = bounded(resource.data);
  const state: 'loading' | 'error' | 'empty' | 'ready' = resource.loading
    ? 'loading' : resource.error ? 'error' : view.records.length === 0 ? 'empty' : 'ready';
  return <Page title="Executions" subtitle="Bounded exec-session catalogue, status and captured output.">
    <Toolbar loading={resource.loading} onRefresh={resource.reload} />
    <ResourceState state={state} loadingLabel="Reading executions…" emptyLabel="No executions"
      emptyDetail="Commands executed in containers will appear here." error={boundedMessage(resource.error)}
      retryLabel="Retry executions" onRetry={resource.reload}>
      {view.records.map((item) => <Card key={`${inventoryVersion}:${item.id}`}
        variant={selected === item.id ? 'filled' : 'outline'}>
        <CardHeader label={item.command?.join(' ') || shortId(item.id)} detail={`container ${shortId(item.container_id)}`} />
        <CardContent>
          <Badge label={item.running ? 'running' : `exited ${item.exit_code}`} tone={item.running ? 'positive' : 'neutral'} />
          {selected === item.id ? <ExecutionDetail inspection={inspection} output={output}
            onRetry={() => inspect(item.id)} /> : null}
        </CardContent>
        <CardActions gap={1}>
          <Button label={selected === item.id ? 'Hide details' : 'Details'} enabled={!busy}
            onInvoke={() => selected === item.id ? setSelected('') : void inspect(item.id)} />
          <Button label={busy === `logs:${item.id}` ? 'Loading logs…' : 'Load output'} enabled={!busy}
            onInvoke={() => void logs(item.id)} />
          <Button label={busy === `wait:${item.id}` ? 'Waiting…' : 'Wait up to 5s'} enabled={!busy && item.running}
            onInvoke={() => void wait(item.id)} />
          <ConfirmAction authorityKey={`execution:${item.id}:SIGTERM`} label="Terminate" confirmLabel="Confirm SIGTERM"
            pendingLabel="Confirm SIGTERM" question={`Send SIGTERM to execution ${item.id}?`}
            enabled={!busy && item.running} onConfirm={() => terminate(item.id)} />
          <ConfirmAction authorityKey={`execution:${item.id}:remove`} label="Remove record" confirmLabel="Confirm removal"
            pendingLabel="Confirm removal" question={`Remove execution record ${shortId(item.id)}?`}
            enabled={!busy && !item.running} onConfirm={() => remove(item.id)} />
        </CardActions>
      </Card>)}
      <Omitted count={view.omitted} />
      {truncated ? <Text label="The host execution catalogue was truncated at its safety limit." color="warning" wrap /> : null}
    </ResourceState>
  </Page>;
}

function ExecutionDetail({ inspection, output, onRetry }: {
  inspection: Inspection; output: Output | null; onRetry: () => void;
}) {
  const detailState = inspection.state === 'idle' ? 'loading'
    : inspection.state === 'ready' && inspection.count === 0 ? 'empty' : inspection.state;
  return <>
    <ResourceState state={detailState} loadingLabel="Reading execution details…" emptyLabel="No execution details"
      emptyDetail="The host returned no inspectable fields." error={boundedMessage(inspection.error)}
      retryLabel="Retry details" onRetry={onRetry}>
      <KeyValueTable source={EXECUTION_DETAIL_SOURCE} schema={DETAIL_SCHEMA}
        height={{ minimum: { step: 10 }, maximum: { step: 28 } }} />
    </ResourceState>
    {output ? <Column gap={1}>
      <Heading label="Standard output" scale="caption" />
      <LogView key={`stdout-${output.revision}`}
        value={output.stdout || (output.eof ? 'No stdout captured (EOF).' : 'No stdout captured yet; execution is still running.')}
        monospace />
      {output.stdoutTruncated ? <Text label="Standard output was truncated to its configured bound." color="warning" /> : null}
      <Heading label="Standard error" scale="caption" />
      <LogView key={`stderr-${output.revision}`}
        value={output.stderr || (output.eof ? 'No stderr captured (EOF).' : 'No stderr captured yet; execution is still running.')}
        monospace />
      {output.stderrTruncated ? <Text label="Standard error was truncated to its configured bound." color="warning" /> : null}
      <Text label={output.eof ? 'Captured output is complete (EOF).' : 'Execution is still running; later output may appear.'} color="text-dim" />
      {output.truncated && !output.stdoutTruncated && !output.stderrTruncated
        ? <Text label="Host output was truncated to its configured bound." color="warning" /> : null}
    </Column> : null}
  </>;
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
