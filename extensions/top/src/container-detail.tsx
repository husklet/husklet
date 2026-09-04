import React from 'react';
import {
  Button, CardContent, ConfirmAction, Entry, Heading, ObjectInspector, ResourceState, Row, Separator, Text,
  type ContainerSummary, type WorkspaceApi,
} from '@husklet/react';
import { LOG_LIMIT, boundedMessage, logText, shortId } from './model.js';

const { useState } = React;
const INSPECTOR_BOUNDS = Object.freeze({ maxDepth: 8, maxNodes: 128, maxStringLength: 256 });

export type Inspection = {
  id: string;
  state: 'idle' | 'loading' | 'ready' | 'error';
  count: number;
  detail: ContainerSummary | null;
  error: unknown;
};
export type LifecycleVerb = 'start' | 'restart' | 'pause' | 'unpause' | 'stop' | 'kill';
export type LifecycleAction = (verb: LifecycleVerb, id: string, signal?: string, generation?: number) => Promise<void>;

function StructuredDetail({ value }: { value: unknown }) {
  return (
    <ObjectInspector
      value={value}
      {...INSPECTOR_BOUNDS}
      height={{ minimum: { step: 10 }, maximum: { step: 32 } }} />
  );
}

type ContainerDetailProps = {
  api: WorkspaceApi;
  container: ContainerSummary;
  act: LifecycleAction;
  inspection: Inspection;
  onRetry: () => void | Promise<void>;
  onOpenExecution?: (id: string) => void | Promise<void>;
};

export function ContainerDetail({ api, container, act, inspection, onRetry, onOpenExecution }: ContainerDetailProps) {
  const [command, setCommand] = useState({ argv: '', user: '', workingDirectory: '' });
  const [execution, setExecution] = useState<{ state: 'idle' | 'loading' | 'ready' | 'error'; id: string; error: unknown }>({ state: 'idle', id: '', error: null });
  const [attachment, setAttachment] = useState<{ state: 'idle' | 'loading' | 'ready' | 'error'; slot: string; error: unknown }>({ state: 'idle', slot: '', error: null });
  const [logs, setLogs] = useState<string | null>(null);
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
    } catch (error: unknown) { setExecution({ state: 'error', id: '', error }); }
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
    } catch (error: unknown) { setAttachment({ state: 'error', slot: '', error }); }
  };
  return (
    <CardContent gap={2}>
      <ResourceState
        state={inspection.state === 'idle' ? 'loading' : inspection.state === 'ready' && inspection.count === 0 ? 'empty' : inspection.state}
        loadingLabel={'Reading container details…'}
        emptyLabel={'No container details'}
        emptyDetail={'The host returned no inspectable fields.'}
        error={boundedMessage(inspection.error)}
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
          onConfirm={() => act('kill', container.id, 'SIGKILL', container.generation)} />
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
        label={boundedMessage(attachment.error)}
        color={'danger'}
        wrap={true} /> : null}
      {attachment.state === 'ready' ? <Text
        label={`Interactive terminal opened in ${attachment.slot}.`}
        color={'positive'}
        wrap={true} /> : null}
      {execution.state === 'error' ? <Text
        label={boundedMessage(execution.error)}
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
