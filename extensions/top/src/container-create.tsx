import React from 'react';
import {
  Button, Card, CardActions, CardContent, CardHeader, Entry, Heading, Row, Spinner, Text,
  type ContainerCreateSpec, type WorkspaceApi,
} from '@husklet/react';
import { boundedMessage } from './model.js';

const { useState } = React;

export type ContainerCreateDraft = {
  image: string;
  name: string;
  hostname: string;
  user: string;
  labels: string;
  network: string;
  entrypoint: string;
  command: string;
  environment: string;
  workingDirectory: string;
  memoryMb: string;
  cpus: string;
  pidsLimit: string;
  mounts: string;
  ports: string;
};

type CreatedContainer = { id: string; name: string };

type ContainerCreateProps = {
  api: WorkspaceApi;
  blocked: boolean;
  onBusyChange: (busy: boolean) => void;
  reload: () => void | Promise<void>;
  options: (draft: ContainerCreateDraft) => Omit<ContainerCreateSpec, 'image' | 'name'>;
};

const emptyDraft = (): ContainerCreateDraft => ({
  image: '', name: '', hostname: '', user: '', labels: '', network: '', entrypoint: '', command: '',
  environment: '', workingDirectory: '', memoryMb: '', cpus: '', pidsLimit: '', mounts: '', ports: '',
});

export function ContainerCreate({ api, blocked, onBusyChange, reload, options }: ContainerCreateProps) {
  const [draft, setDraft] = useState<ContainerCreateDraft>(emptyDraft);
  const [created, setCreated] = useState<CreatedContainer | null>(null);
  const [error, setError] = useState<unknown>(null);
  const [notice, setNotice] = useState('');
  let configurationError = '';
  try {
    options(draft);
  } catch (cause: unknown) {
    configurationError = boundedMessage(cause);
  }
  const update = (field: keyof ContainerCreateDraft, value: unknown) => {
    setDraft((current) => ({ ...current, [field]: String(value ?? '') }));
  };
  const createAndStart = async () => {
    if (blocked) return;
    onBusyChange(true);
    setError(null);
    setNotice('');
    let target = created;
    try {
      if (!target) {
        const name = draft.name.trim();
        const id = await api.containers.create({ image: draft.image.trim(), name, ...options(draft) });
        target = { id, name };
        setCreated(target);
      }
      await api.containers.start(target.id);
      setNotice(`Created and started ${target.name}.`);
      setCreated(null);
      setDraft(emptyDraft());
      await reload();
    } catch (cause: unknown) {
      setError(cause);
    } finally {
      onBusyChange(false);
    }
  };
  const editable = !created && !blocked;
  return (
    <Card variant={'outline'}>
      <CardHeader label={'Create a container'} detail={'Uses a local image and starts it after durable creation.'} />
      <CardContent gap={1}>
        <Heading label={'Identity and image'} scale={'body'} />
        <Row gap={1} wrap={true}>
          <Entry value={draft.image} placeholder={'Image reference'} enabled={editable} onChange={(event) => update('image', event.value)} />
          <Entry value={draft.name} placeholder={'Container name'} enabled={editable} onChange={(event) => update('name', event.value)} />
          <Entry value={draft.hostname} placeholder={'Hostname (optional)'} enabled={editable} onChange={(event) => update('hostname', event.value)} />
          <Entry value={draft.user} placeholder={'Run as user (optional)'} enabled={editable} onChange={(event) => update('user', event.value)} />
          <Entry value={draft.labels} placeholder={'Labels JSON (optional)'} enabled={editable} onChange={(event) => update('labels', event.value)} />
        </Row>
        <Text label={'Labels use JSON [name, value] pairs, for example [["role","worker"]].'} color={'text-dim'} wrap={true} />
        <Heading label={'Process'} scale={'body'} />
        <Row gap={1} wrap={true}>
          <Entry value={draft.entrypoint} placeholder={'Entrypoint argv JSON (optional)'} enabled={editable} onChange={(event) => update('entrypoint', event.value)} />
          <Entry value={draft.command} placeholder={'Command argv JSON (optional)'} enabled={editable} onChange={(event) => update('command', event.value)} />
          <Entry value={draft.environment} placeholder={'Environment pairs JSON (optional)'} enabled={editable} onChange={(event) => update('environment', event.value)} />
          <Entry value={draft.workingDirectory} placeholder={'Working directory (optional)'} enabled={editable} onChange={(event) => update('workingDirectory', event.value)} />
        </Row>
        <Text label={'Entrypoint and command use JSON argv arrays; environment uses JSON [name, value] pairs.'} color={'text-dim'} wrap={true} />
        <Heading label={'Resources and connectivity'} scale={'body'} />
        <Row gap={1} wrap={true}>
          <Entry value={draft.memoryMb} placeholder={'Memory limit MiB (optional)'} enabled={editable} onChange={(event) => update('memoryMb', event.value)} />
          <Entry value={draft.cpus} placeholder={'CPU limit (optional)'} enabled={editable} onChange={(event) => update('cpus', event.value)} />
          <Entry value={draft.pidsLimit} placeholder={'PID limit (optional)'} enabled={editable} onChange={(event) => update('pidsLimit', event.value)} />
          <Entry value={draft.network} placeholder={'Initial network (optional)'} enabled={editable} onChange={(event) => update('network', event.value)} />
          <Entry value={draft.mounts} placeholder={'Named volume mounts JSON (optional)'} enabled={editable} onChange={(event) => update('mounts', event.value)} />
          <Entry value={draft.ports} placeholder={'Published ports JSON (optional)'} enabled={editable} onChange={(event) => update('ports', event.value)} />
        </Row>
        <Text label={'Mounts and ports use JSON object arrays; host filesystem paths and host addresses are not accepted.'} color={'text-dim'} wrap={true} />
      </CardContent>
      <CardActions>
        {blocked ? <Spinner /> : null}
        <Button
          label={created ? 'Retry start' : blocked ? 'Creating…' : 'Create and start'}
          enabled={!blocked && (created !== null || (draft.image.trim().length > 0 && draft.name.trim().length > 0 && !configurationError))}
          onInvoke={createAndStart} />
      </CardActions>
      {configurationError ? <Text label={configurationError} color={'danger'} wrap={true} /> : null}
      {error ? <Text label={boundedMessage(error)} color={'danger'} wrap={true} /> : null}
      {notice ? <Text label={notice} color={'positive'} wrap={true} /> : null}
    </Card>
  );
}
