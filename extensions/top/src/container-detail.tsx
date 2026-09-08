import React from 'react';
import {
  Button,
  CardContent,
  Column,
  ConfirmAction,
  Entry,
  Expander,
  FormControl,
  FormLabel,
  Heading,
  Badge,
  IconButton,
  ResourceState,
  Row,
  Separator,
  Text,
  type ContainerSummary,
  type WorkspaceApi,
} from '@husklet/react';
import { LOG_LIMIT, boundedMessage, logText, shortId } from './model.js';

const { useState } = React;

const bytes = (value: string) => new TextEncoder().encode(value).length;

function commandArgv(program: string, arguments_: string[]): string[] {
  const argv = [program, ...arguments_];
  if (!argv[0]) throw new Error('Enter a program to run.');
  if (
    argv.length > 64 ||
    argv.some((argument) => argument.includes('\0') || bytes(argument) > 4_096) ||
    argv.reduce((total, argument) => total + bytes(argument), 0) > 32_768
  ) {
    throw new Error(
      'Command must contain at most 64 NUL-free arguments, each at most 4096 bytes and 32768 bytes in total.',
    );
  }
  return argv;
}

function CommandEditor({
  program,
  arguments_,
  enabled,
  onProgramChange,
  onArgumentsChange,
}: {
  program: string;
  arguments_: string[];
  enabled: boolean;
  onProgramChange: (value: string) => void;
  onArgumentsChange: (value: string[]) => void;
}) {
  return (
    <Column gap={1} width={{ maximum: { chars: 72 } }}>
      <FormControl gap={1}>
        <FormLabel label={'Program'} />
        <Entry
          value={program}
          placeholder={'Program, e.g. sh'}
          enabled={enabled}
          onChange={(event) => onProgramChange(String(event.value ?? ''))}
        />
      </FormControl>
      <FormControl gap={1}>
        <Heading label={'Arguments'} scale={'caption'} />
        <Row gap={1} align={'center'} wrap={true} width={'content'}>
          <Button
            label={'Add argument'}
            variant={'ghost'}
            enabled={enabled && arguments_.length < 63}
            onInvoke={() => onArgumentsChange([...arguments_, ''])}
          />
        </Row>
        {arguments_.length === 0 ? (
          <Text label={'No arguments'} color={'text-dim'} />
        ) : (
          <Column gap={1}>
            {arguments_.map((argument, index) => (
              <Row key={index} gap={1} align={'center'}>
                <Text label={`${index + 1}`} color={'text-dim'} width={{ chars: 2 }} />
                <Entry
                  value={argument}
                  placeholder={`Argument ${index + 1}`}
                  grow={true}
                  enabled={enabled}
                  onChange={(event) =>
                    onArgumentsChange(
                      arguments_.map((held, at) =>
                        at === index ? String(event.value ?? '') : held,
                      ),
                    )
                  }
                />
                <IconButton
                  icon={'user-trash-symbolic'}
                  label={`Remove argument ${index + 1}`}
                  tooltip={`Remove argument ${index + 1}`}
                  variant={'ghost'}
                  enabled={enabled}
                  onInvoke={() => onArgumentsChange(arguments_.filter((_, at) => at !== index))}
                />
              </Row>
            ))}
          </Column>
        )}
      </FormControl>
    </Column>
  );
}

export type Inspection = {
  id: string;
  state: 'idle' | 'loading' | 'ready' | 'error';
  count: number;
  detail: ContainerSummary | null;
  error: unknown;
};
export type LifecycleVerb = 'start' | 'restart' | 'pause' | 'unpause' | 'stop' | 'kill';
export type LifecycleAction = (
  verb: LifecycleVerb,
  id: string,
  signal?: string,
  generation?: number,
) => Promise<void>;

function StructuredDetail({ value }: { value: ContainerSummary | null }) {
  if (!value) return null;
  return (
    <Column gap={1}>
      <Heading label="Container details" scale="caption" />
      <Row gap={1} wrap>
        <Badge label={`State · ${value.state}`} />
        <Text label={`Name · ${value.name || 'Unnamed'}`} />
      </Row>
      <Text label={`Image · ${value.image}`} wrap />
      <Text label={`Immutable container ID · ${value.id}`} color="text-dim" wrap />
      <Text label={`Created · ${value.created}`} color="text-dim" />
      <Text label={`Generation · ${value.generation ?? 'Unavailable'}`} color="text-dim" />
    </Column>
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

export function ContainerDetail({
  api,
  container,
  act,
  inspection,
  onRetry,
  onOpenExecution,
}: ContainerDetailProps) {
  const [command, setCommand] = useState({
    program: '',
    arguments: [] as string[],
    user: '',
    workingDirectory: '',
  });
  const [execution, setExecution] = useState<{
    state: 'idle' | 'loading' | 'ready' | 'error';
    id: string;
    error: unknown;
  }>({ state: 'idle', id: '', error: null });
  const [attachment, setAttachment] = useState<{
    state: 'idle' | 'loading' | 'ready' | 'error';
    slot: string;
    error: unknown;
  }>({ state: 'idle', slot: '', error: null });
  const [logs, setLogs] = useState<string | null>(null);
  const run = async () => {
    setExecution({ state: 'loading', id: '', error: null });
    try {
      const argv = commandArgv(command.program, command.arguments);
      if (command.user.length > 4_096 || command.workingDirectory.length > 4_096) {
        throw new Error('User and working directory must each be at most 4096 characters.');
      }
      const id = await api.containers.exec(container.id, container.generation, {
        command: argv,
        ...(command.user.trim() ? { user: command.user.trim() } : {}),
        ...(command.workingDirectory.trim()
          ? { workingDirectory: command.workingDirectory.trim() }
          : {}),
      });
      setExecution({ state: 'ready', id, error: null });
    } catch (error: unknown) {
      setExecution({ state: 'error', id: '', error });
    }
  };
  const readLogs = async () =>
    setLogs(
      logText(await api.containers.logs(container.id, { stdout: true, stderr: true })).slice(
        -LOG_LIMIT * 160,
      ),
    );
  const attach = async () => {
    setAttachment({ state: 'loading', slot: '', error: null });
    try {
      const argv = commandArgv(command.program, command.arguments);
      const slot = await api.containers.attachTerminal(container.id, argv);
      setAttachment({ state: 'ready', slot, error: null });
    } catch (error: unknown) {
      setAttachment({ state: 'error', slot: '', error });
    }
  };
  return (
    <CardContent gap={2}>
      <ResourceState
        state={
          inspection.state === 'idle'
            ? 'loading'
            : inspection.state === 'ready' && inspection.count === 0
              ? 'empty'
              : inspection.state
        }
        loadingLabel={'Reading container details…'}
        emptyLabel={'No container details'}
        emptyDetail={'The host returned no inspectable fields.'}
        error={boundedMessage(inspection.error)}
        retryLabel={'Retry details'}
        onRetry={onRetry}
      >
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
          onConfirm={() => act('kill', container.id, 'SIGKILL', container.generation)}
        />
      </Row>
      {logs === null ? null : <Text label={logs || 'No log output.'} wrap={true} />}
      <Separator />
      <Heading label={'Run a command'} scale={'caption'} />
      <Text
        label={
          'Execute captures output for later inspection. Attach terminal opens the same command interactively.'
        }
        color={'text-dim'}
        wrap={true}
      />
      <CommandEditor
        program={command.program}
        arguments_={command.arguments}
        enabled={execution.state !== 'loading' && attachment.state !== 'loading'}
        onProgramChange={(program) => setCommand((value) => ({ ...value, program }))}
        onArgumentsChange={(arguments_) =>
          setCommand((value) => ({ ...value, arguments: arguments_ }))
        }
      />
      <Expander label={'Execution options'}>
        <Column gap={1}>
          <Entry
            value={command.user}
            placeholder={'Run as user (optional)'}
            enabled={execution.state !== 'loading'}
            onChange={(event) =>
              setCommand((value) => ({ ...value, user: String(event.value ?? '') }))
            }
          />
          <Entry
            value={command.workingDirectory}
            placeholder={'Working directory (optional)'}
            enabled={execution.state !== 'loading'}
            onChange={(event) =>
              setCommand((value) => ({ ...value, workingDirectory: String(event.value ?? '') }))
            }
          />
        </Column>
      </Expander>
      <Row gap={1} wrap={true}>
        <Button
          label={execution.state === 'loading' ? 'Executing…' : 'Execute'}
          enabled={execution.state !== 'loading' && command.program.trim().length > 0}
          onInvoke={run}
        />
        <Button
          label={attachment.state === 'loading' ? 'Attaching…' : 'Attach terminal'}
          enabled={
            attachment.state !== 'loading' &&
            command.program.trim().length > 0 &&
            container.state === 'running'
          }
          onInvoke={attach}
        />
      </Row>
      {attachment.state === 'error' ? (
        <Text label={boundedMessage(attachment.error)} color={'danger'} wrap={true} />
      ) : null}
      {attachment.state === 'ready' ? (
        <Text
          label={`Interactive terminal opened in ${attachment.slot}.`}
          color={'positive'}
          wrap={true}
        />
      ) : null}
      {execution.state === 'error' ? (
        <Text label={boundedMessage(execution.error)} color={'danger'} wrap={true} />
      ) : null}
      {execution.state === 'ready' ? (
        <Row gap={1} wrap={true} align={'center'}>
          <Text label={`Execution ${execution.id} created.`} color={'positive'} wrap={true} />
          {onOpenExecution ? (
            <Button label={'Inspect execution'} onInvoke={() => onOpenExecution(execution.id)} />
          ) : null}
        </Row>
      ) : null}
    </CardContent>
  );
}
