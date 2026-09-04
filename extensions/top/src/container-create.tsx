import React from 'react';
import {
  Button, Card, CardActions, CardContent, CardHeader, Entry, Expander, Heading, Row, Spinner, Text,
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
};

const emptyDraft = (): ContainerCreateDraft => ({
  image: '', name: '', hostname: '', user: '', labels: '', network: '', entrypoint: '', command: '',
  environment: '', workingDirectory: '', memoryMb: '', cpus: '', pidsLimit: '', mounts: '', ports: '',
});

export function containerCreateOptions(draft: ContainerCreateDraft): Omit<ContainerCreateSpec, 'image' | 'name'> {
  const bytes = (value: string) => new TextEncoder().encode(value).byteLength;
  const hostname = draft.hostname;
  if (hostname && (bytes(hostname) > 253 || !/^[A-Za-z0-9][A-Za-z0-9_.-]*$/.test(hostname))) {
    throw new Error('Hostname must start with an ASCII letter or digit, contain only ASCII letters, digits, dots, underscores or hyphens, and be at most 253 bytes.');
  }
  const user = draft.user;
  if (user && (bytes(user) > 256 || user.includes('\0'))) {
    throw new Error('Run as user must be a nonempty, NUL-free value of at most 256 bytes.');
  }
  const labelsText = draft.labels.trim();
  let labels;
  if (labelsText) {
    try { labels = JSON.parse(labelsText); } catch { throw new Error('Labels must be valid JSON pairs, such as [["role","worker"]].'); }
    if (!Array.isArray(labels) || labels.length > 128
      || labels.some((pair: unknown) => !Array.isArray(pair) || pair.length !== 2
        || pair.some((value: unknown) => typeof value !== 'string')
        || pair[0].length === 0 || pair[0].includes('\0') || bytes(pair[0]) > 256
        || pair[1].includes('\0') || bytes(pair[1]) > 4_096)
      || new Set(labels.map(([name]) => name)).size !== labels.length) {
      throw new Error('Labels must contain at most 128 unique [name, value] pairs; names are nonempty and at most 256 bytes, values at most 4096 bytes, and both are NUL-free.');
    }
  }
  const network = draft.network;
  if (network && (bytes(network) > 255 || !/^[A-Za-z0-9][A-Za-z0-9_.-]*$/.test(network))) {
    throw new Error('Initial network must start with an ASCII letter or digit, contain only ASCII letters, digits, dots, underscores or hyphens, and be at most 255 bytes.');
  }
  const entrypointText = draft.entrypoint.trim();
  let entrypoint;
  if (entrypointText) {
    try { entrypoint = JSON.parse(entrypointText); } catch { throw new Error('Entrypoint must be valid JSON, such as ["/bin/sh","-lc"].'); }
    if (!Array.isArray(entrypoint) || entrypoint.length === 0 || entrypoint.length > 64 || entrypoint[0] === ''
      || entrypoint.some((argument) => typeof argument !== 'string' || argument.includes('\0') || bytes(argument) > 4_096)
      || entrypoint.reduce((total, argument) => total + bytes(argument), 0) > 32_768) {
      throw new Error('Entrypoint must contain 1 to 64 NUL-free string arguments, each at most 4096 bytes and 32768 bytes in total.');
    }
  }
  const commandText = draft.command.trim();
  let command;
  if (commandText) {
    try { command = JSON.parse(commandText); } catch { throw new Error('Command must be valid JSON, such as ["sh","-lc","printf ready"].'); }
    if (!Array.isArray(command) || command.length > 64 || (command.length > 0 && command[0] === '')
      || command.some((argument) => typeof argument !== 'string' || argument.includes('\0') || bytes(argument) > 4_096)
      || command.reduce((total, argument) => total + bytes(argument), 0) > 32_768) {
      throw new Error('Command must contain at most 64 NUL-free string arguments, each at most 4096 bytes and 32768 bytes in total.');
    }
  }
  if ([...(entrypoint ?? []), ...(command ?? [])].reduce((total, argument) => total + bytes(argument), 0) > 32_768) {
    throw new Error('Entrypoint and command together must contain at most 32768 bytes.');
  }
  const environmentText = draft.environment.trim();
  let environment;
  if (environmentText) {
    try { environment = JSON.parse(environmentText); } catch { throw new Error('Environment must be valid JSON pairs, such as [["MODE","test"]].'); }
    if (!Array.isArray(environment) || environment.length > 256
      || environment.some((pair) => !Array.isArray(pair) || pair.length !== 2
        || pair.some((value) => typeof value !== 'string')
        || pair[0].length === 0 || pair[0].includes('=') || pair[0].includes('\0') || bytes(pair[0]) > 256
        || pair[1].includes('\0') || bytes(pair[1]) > 8_192)
      || new Set(environment.map(([name]) => name)).size !== environment.length) {
      throw new Error('Environment must contain at most 256 unique [name, value] pairs with bounded NUL-free strings.');
    }
  }
  const workingDirectory = draft.workingDirectory.trim();
  if (workingDirectory && (!workingDirectory.startsWith('/') || bytes(workingDirectory) > 4_096
    || workingDirectory.includes('\0') || workingDirectory.split('/').some((part: string) => part === '.' || part === '..'))) {
    throw new Error('Working directory must be an absolute, NUL-free path without dot segments and at most 4096 bytes.');
  }
  const memoryMb = optionalDecimalLimit(draft.memoryMb, 'Memory limit', 1_048_576);
  const cpus = optionalDecimalLimit(draft.cpus, 'CPU limit', 256);
  const pidsLimit = optionalDecimalLimit(draft.pidsLimit, 'PID limit', 1_000_000);
  const mountsText = draft.mounts.trim();
  let mounts;
  if (mountsText) {
    try { mounts = JSON.parse(mountsText); } catch { throw new Error('Mounts must be valid JSON, such as [{"volume":"cache","target":"/cache","read_only":true}].'); }
    const allowed = new Set(['volume', 'target', 'read_only']);
    if (!Array.isArray(mounts) || mounts.length > 64
      || mounts.some((mount: any) => !mount || typeof mount !== 'object' || Array.isArray(mount)
        || Object.keys(mount).some((key) => !allowed.has(key))
        || typeof mount.volume !== 'string' || !/^[A-Za-z0-9][A-Za-z0-9_.-]{0,254}$/.test(mount.volume)
        || typeof mount.target !== 'string' || !mount.target.startsWith('/') || bytes(mount.target) > 4_096
        || mount.target.includes('\0') || mount.target.split('/').some((part: string) => part === '.' || part === '..')
        || (mount.read_only !== undefined && typeof mount.read_only !== 'boolean'))
      || new Set(mounts.map((mount) => mount.target)).size !== mounts.length) {
      throw new Error('Mounts must contain at most 64 named volumes with unique absolute targets and optional boolean read_only. Host bind mounts are not accepted.');
    }
    mounts = mounts.map(({ volume, target, read_only = false }) => ({ volume, target, read_only }));
  }
  const portsText = draft.ports.trim();
  let ports;
  if (portsText) {
    try { ports = JSON.parse(portsText); } catch { throw new Error('Ports must be valid JSON, such as [{"container":8080,"host":18080,"protocol":"tcp"}].'); }
    const allowed = new Set(['container', 'host', 'protocol']);
    const validPort = (value: unknown): value is number => Number.isInteger(value) && Number(value) >= 1 && Number(value) <= 65_535;
    if (!Array.isArray(ports) || ports.length > 64
      || ports.some((port: any) => !port || typeof port !== 'object' || Array.isArray(port)
        || Object.keys(port).some((key) => !allowed.has(key))
        || !validPort(port.container) || (port.host !== undefined && port.host !== null && !validPort(port.host))
        || !['tcp', 'udp'].includes(port.protocol))
      || new Set(ports.map((port) => `${port.container}/${port.protocol}`)).size !== ports.length) {
      throw new Error('Ports must contain at most 64 unique container-port/protocol pairs from 1 to 65535; host is an optional port number, not an address.');
    }
    ports = ports.map(({ container, host = null, protocol }) => ({ container, host, protocol }));
  }
  return {
    ...(hostname ? { hostname } : {}),
    ...(entrypoint ? { entrypoint } : {}),
    ...(command ? { command } : {}),
    ...(environment ? { environment } : {}),
    ...(workingDirectory ? { working_directory: workingDirectory } : {}),
    ...(user ? { user } : {}),
    ...(labels ? { labels } : {}),
    ...(network ? { network } : {}),
    ...(memoryMb === null ? {} : { memory_mb: memoryMb }),
    ...(cpus === null ? {} : { cpus }),
    ...(pidsLimit === null ? {} : { pids_limit: pidsLimit }),
    ...(mounts ? { mounts } : {}),
    ...(ports ? { ports } : {}),
  };
}

function optionalDecimalLimit(value: string, label: string, maximum: number): number | null {
  const text = value.trim();
  if (!text) return null;
  if (!/^[0-9]+$/.test(text)) throw new Error(`${label} must be a whole decimal number from 1 to ${maximum}.`);
  const parsed = Number(text);
  if (!Number.isSafeInteger(parsed) || parsed < 1 || parsed > maximum) {
    throw new Error(`${label} must be a whole decimal number from 1 to ${maximum}.`);
  }
  return parsed;
}

export function ContainerCreate({ api, blocked, onBusyChange, reload }: ContainerCreateProps) {
  const [draft, setDraft] = useState<ContainerCreateDraft>(emptyDraft);
  const [created, setCreated] = useState<CreatedContainer | null>(null);
  const [error, setError] = useState<unknown>(null);
  const [notice, setNotice] = useState('');
  let configurationError = '';
  try {
    containerCreateOptions(draft);
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
        const id = await api.containers.create({ image: draft.image.trim(), name, ...containerCreateOptions(draft) });
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
    <Expander label={'Create a container'}>
      <Card variant={'outline'}>
        <CardHeader label={'New container'} detail={'Uses a local image and starts it after durable creation.'} />
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
    </Expander>
  );
}
