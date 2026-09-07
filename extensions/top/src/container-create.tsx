import React from 'react';
import {
  Button,
  Card,
  CardActions,
  CardContent,
  CardHeader,
  Entry,
  Expander,
  Heading,
  Row,
  Spinner,
  Text,
  type ContainerCreateSpec,
  type WorkspaceApi,
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

type CreatedContainer = { id: string; name: string; generation: number };

type ContainerCreateProps = {
  api: WorkspaceApi;
  blocked: boolean;
  onBusyChange: (busy: boolean) => void;
  reload: () => void | Promise<void>;
};

const emptyDraft = (): ContainerCreateDraft => ({
  image: '',
  name: '',
  hostname: '',
  user: '',
  labels: '',
  network: '',
  entrypoint: '',
  command: '',
  environment: '',
  workingDirectory: '',
  memoryMb: '',
  cpus: '',
  pidsLimit: '',
  mounts: '',
  ports: '',
});

const byteLength = (value: string) => new TextEncoder().encode(value).byteLength;

function parseJson(text: string, message: string): unknown {
  try {
    return JSON.parse(text);
  } catch {
    throw new Error(message);
  }
}

function isStringPair(value: unknown): value is [string, string] {
  return (
    Array.isArray(value) && value.length === 2 && value.every((item) => typeof item === 'string')
  );
}

function hasUniqueNames(pairs: [string, string][]): boolean {
  return new Set(pairs.map(([name]) => name)).size === pairs.length;
}

type VolumeMount = NonNullable<ContainerCreateSpec['mounts']>[number];
type PublishedPort = NonNullable<ContainerCreateSpec['ports']>[number];

function isObject(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === 'object' && !Array.isArray(value);
}

function isValidVolumeMount(value: unknown, allowed: Set<string>): value is VolumeMount {
  if (!isObject(value) || Object.keys(value).some((key) => !allowed.has(key))) {
    return false;
  }
  const { volume, target, read_only: readOnly } = value;
  return (
    typeof volume === 'string' &&
    /^[A-Za-z0-9][A-Za-z0-9_.-]{0,254}$/.test(volume) &&
    typeof target === 'string' &&
    target.startsWith('/') &&
    byteLength(target) <= 4_096 &&
    !target.includes('\0') &&
    !target.split('/').some((part) => part === '.' || part === '..') &&
    (readOnly === undefined || typeof readOnly === 'boolean')
  );
}

function isPortNumber(value: unknown): value is number {
  return Number.isInteger(value) && Number(value) >= 1 && Number(value) <= 65_535;
}

function isValidPublishedPort(value: unknown, allowed: Set<string>): value is PublishedPort {
  if (!isObject(value) || Object.keys(value).some((key) => !allowed.has(key))) {
    return false;
  }
  const { container, host, protocol } = value;
  return (
    isPortNumber(container) &&
    (host === undefined || host === null || isPortNumber(host)) &&
    (protocol === 'tcp' || protocol === 'udp')
  );
}

function parseLabels(text: string): [string, string][] | undefined {
  if (!text) {
    return undefined;
  }

  const value = parseJson(text, 'Labels must be valid JSON pairs, such as [["role","worker"]].');
  const valid =
    Array.isArray(value) &&
    value.length <= 128 &&
    value.every(
      (pair): pair is [string, string] =>
        isStringPair(pair) &&
        pair[0].length > 0 &&
        !pair[0].includes('\0') &&
        byteLength(pair[0]) <= 256 &&
        !pair[1].includes('\0') &&
        byteLength(pair[1]) <= 4_096,
    ) &&
    hasUniqueNames(value);

  if (!valid) {
    throw new Error(
      'Labels must contain at most 128 unique [name, value] pairs; names are nonempty and at most 256 bytes, values at most 4096 bytes, and both are NUL-free.',
    );
  }
  return value;
}

function parseArguments(text: string, kind: 'Command' | 'Entrypoint'): string[] | undefined {
  if (!text) {
    return undefined;
  }

  const example = kind === 'Entrypoint' ? '["/bin/sh","-lc"]' : '["sh","-lc","printf ready"]';
  const value = parseJson(text, `${kind} must be valid JSON, such as ${example}.`);
  const validCount =
    Array.isArray(value) &&
    value.length <= 64 &&
    (kind === 'Command' || value.length > 0) &&
    (value.length === 0 || value[0] !== '');
  const validArguments =
    validCount &&
    value.every(
      (argument): argument is string =>
        typeof argument === 'string' && !argument.includes('\0') && byteLength(argument) <= 4_096,
    );
  const validTotal =
    validArguments && value.reduce((total, argument) => total + byteLength(argument), 0) <= 32_768;

  if (!validTotal) {
    const count = kind === 'Entrypoint' ? '1 to 64' : 'at most 64';
    throw new Error(
      `${kind} must contain ${count} NUL-free string arguments, each at most 4096 bytes and 32768 bytes in total.`,
    );
  }
  return value;
}

function parseEnvironment(text: string): [string, string][] | undefined {
  if (!text) {
    return undefined;
  }

  const value = parseJson(text, 'Environment must be valid JSON pairs, such as [["MODE","test"]].');
  const valid =
    Array.isArray(value) &&
    value.length <= 256 &&
    value.every(
      (pair): pair is [string, string] =>
        isStringPair(pair) &&
        pair[0].length > 0 &&
        !pair[0].includes('=') &&
        !pair[0].includes('\0') &&
        byteLength(pair[0]) <= 256 &&
        !pair[1].includes('\0') &&
        byteLength(pair[1]) <= 8_192,
    ) &&
    hasUniqueNames(value);

  if (!valid) {
    throw new Error(
      'Environment must contain at most 256 unique [name, value] pairs with bounded NUL-free strings.',
    );
  }
  return value;
}

export function containerCreateOptions(
  draft: ContainerCreateDraft,
): Omit<ContainerCreateSpec, 'image' | 'name'> {
  const hostname = draft.hostname;
  if (hostname && (byteLength(hostname) > 253 || !/^[A-Za-z0-9][A-Za-z0-9_.-]*$/.test(hostname))) {
    throw new Error(
      'Hostname must start with an ASCII letter or digit, contain only ASCII letters, digits, dots, underscores or hyphens, and be at most 253 bytes.',
    );
  }
  const user = draft.user;
  if (user && (byteLength(user) > 256 || user.includes('\0'))) {
    throw new Error('Run as user must be a nonempty, NUL-free value of at most 256 bytes.');
  }
  const labels = parseLabels(draft.labels.trim());
  const network = draft.network;
  if (network && (byteLength(network) > 255 || !/^[A-Za-z0-9][A-Za-z0-9_.-]*$/.test(network))) {
    throw new Error(
      'Initial network must start with an ASCII letter or digit, contain only ASCII letters, digits, dots, underscores or hyphens, and be at most 255 bytes.',
    );
  }
  const entrypoint = parseArguments(draft.entrypoint.trim(), 'Entrypoint');
  const command = parseArguments(draft.command.trim(), 'Command');
  if (
    [...(entrypoint ?? []), ...(command ?? [])].reduce(
      (total, argument) => total + byteLength(argument),
      0,
    ) > 32_768
  ) {
    throw new Error('Entrypoint and command together must contain at most 32768 bytes.');
  }
  const environment = parseEnvironment(draft.environment.trim());
  const workingDirectory = draft.workingDirectory.trim();
  if (
    workingDirectory &&
    (!workingDirectory.startsWith('/') ||
      byteLength(workingDirectory) > 4_096 ||
      workingDirectory.includes('\0') ||
      workingDirectory.split('/').some((part: string) => part === '.' || part === '..'))
  ) {
    throw new Error(
      'Working directory must be an absolute, NUL-free path without dot segments and at most 4096 bytes.',
    );
  }
  const memoryMb = optionalDecimalLimit(draft.memoryMb, 'Memory limit', 1_048_576);
  const cpus = optionalDecimalLimit(draft.cpus, 'CPU limit', 256);
  const pidsLimit = optionalDecimalLimit(draft.pidsLimit, 'PID limit', 1_000_000);
  const mountsText = draft.mounts.trim();
  let mounts;
  if (mountsText) {
    try {
      mounts = JSON.parse(mountsText);
    } catch {
      throw new Error(
        'Mounts must be valid JSON, such as [{"volume":"cache","target":"/cache","read_only":true}].',
      );
    }
    const allowed = new Set(['volume', 'target', 'read_only']);
    if (
      !Array.isArray(mounts) ||
      mounts.length > 64 ||
      mounts.some((mount) => !isValidVolumeMount(mount, allowed)) ||
      new Set(mounts.map((mount) => mount.target)).size !== mounts.length
    ) {
      throw new Error(
        'Mounts must contain at most 64 named volumes with unique absolute targets and optional boolean read_only. Host bind mounts are not accepted.',
      );
    }
    mounts = (mounts as VolumeMount[]).map(({ volume, target, read_only = false }) => ({
      volume,
      target,
      read_only,
    }));
  }
  const portsText = draft.ports.trim();
  let ports;
  if (portsText) {
    try {
      ports = JSON.parse(portsText);
    } catch {
      throw new Error(
        'Ports must be valid JSON, such as [{"container":8080,"host":18080,"protocol":"tcp"}].',
      );
    }
    const allowed = new Set(['container', 'host', 'protocol']);
    if (
      !Array.isArray(ports) ||
      ports.length > 64 ||
      ports.some((port) => !isValidPublishedPort(port, allowed)) ||
      new Set(ports.map((port) => `${port.container}/${port.protocol}`)).size !== ports.length
    ) {
      throw new Error(
        'Ports must contain at most 64 unique container-port/protocol pairs from 1 to 65535; host is an optional port number, not an address.',
      );
    }
    ports = (ports as PublishedPort[]).map(({ container, host = null, protocol }) => ({
      container,
      host,
      protocol,
    }));
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
  if (!/^[0-9]+$/.test(text))
    throw new Error(`${label} must be a whole decimal number from 1 to ${maximum}.`);
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
        const id = await api.containers.create({
          image: draft.image.trim(),
          name,
          ...containerCreateOptions(draft),
        });
        const observed = await api.containers.inspect(id);
        if (observed.id !== id) {
          throw new Error(`Created container ${id} could not be verified by immutable identity.`);
        }
        target = { id, name, generation: observed.generation };
        setCreated(target);
      }
      await api.containers.start(target.id, target.generation);
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
        <CardHeader
          label={'New container'}
          detail={'Uses a local image and starts it after durable creation.'}
        />
        <CardContent gap={1}>
          <Heading label={'Identity and image'} scale={'body'} />
          <Row gap={1} wrap={true}>
            <Entry
              value={draft.image}
              placeholder={'Image reference'}
              enabled={editable}
              onChange={(event) => update('image', event.value)}
            />
            <Entry
              value={draft.name}
              placeholder={'Container name'}
              enabled={editable}
              onChange={(event) => update('name', event.value)}
            />
            <Entry
              value={draft.hostname}
              placeholder={'Hostname (optional)'}
              enabled={editable}
              onChange={(event) => update('hostname', event.value)}
            />
            <Entry
              value={draft.user}
              placeholder={'Run as user (optional)'}
              enabled={editable}
              onChange={(event) => update('user', event.value)}
            />
            <Entry
              value={draft.labels}
              placeholder={'Labels JSON (optional)'}
              enabled={editable}
              onChange={(event) => update('labels', event.value)}
            />
          </Row>
          <Text
            label={'Labels use JSON [name, value] pairs, for example [["role","worker"]].'}
            color={'text-dim'}
            wrap={true}
          />
          <Heading label={'Process'} scale={'body'} />
          <Row gap={1} wrap={true}>
            <Entry
              value={draft.entrypoint}
              placeholder={'Entrypoint argv JSON (optional)'}
              enabled={editable}
              onChange={(event) => update('entrypoint', event.value)}
            />
            <Entry
              value={draft.command}
              placeholder={'Command argv JSON (optional)'}
              enabled={editable}
              onChange={(event) => update('command', event.value)}
            />
            <Entry
              value={draft.environment}
              placeholder={'Environment pairs JSON (optional)'}
              enabled={editable}
              onChange={(event) => update('environment', event.value)}
            />
            <Entry
              value={draft.workingDirectory}
              placeholder={'Working directory (optional)'}
              enabled={editable}
              onChange={(event) => update('workingDirectory', event.value)}
            />
          </Row>
          <Text
            label={
              'Entrypoint and command use JSON argv arrays; environment uses JSON [name, value] pairs.'
            }
            color={'text-dim'}
            wrap={true}
          />
          <Heading label={'Resources and connectivity'} scale={'body'} />
          <Row gap={1} wrap={true}>
            <Entry
              value={draft.memoryMb}
              placeholder={'Memory limit MiB (optional)'}
              enabled={editable}
              onChange={(event) => update('memoryMb', event.value)}
            />
            <Entry
              value={draft.cpus}
              placeholder={'CPU limit (optional)'}
              enabled={editable}
              onChange={(event) => update('cpus', event.value)}
            />
            <Entry
              value={draft.pidsLimit}
              placeholder={'PID limit (optional)'}
              enabled={editable}
              onChange={(event) => update('pidsLimit', event.value)}
            />
            <Entry
              value={draft.network}
              placeholder={'Initial network (optional)'}
              enabled={editable}
              onChange={(event) => update('network', event.value)}
            />
            <Entry
              value={draft.mounts}
              placeholder={'Named volume mounts JSON (optional)'}
              enabled={editable}
              onChange={(event) => update('mounts', event.value)}
            />
            <Entry
              value={draft.ports}
              placeholder={'Published ports JSON (optional)'}
              enabled={editable}
              onChange={(event) => update('ports', event.value)}
            />
          </Row>
          <Text
            label={
              'Mounts and ports use JSON object arrays; host filesystem paths and host addresses are not accepted.'
            }
            color={'text-dim'}
            wrap={true}
          />
        </CardContent>
        <CardActions>
          {blocked ? <Spinner /> : null}
          <Button
            label={created ? 'Retry start' : blocked ? 'Creating…' : 'Create and start'}
            enabled={
              !blocked &&
              (created !== null ||
                (draft.image.trim().length > 0 &&
                  draft.name.trim().length > 0 &&
                  !configurationError))
            }
            onInvoke={createAndStart}
          />
        </CardActions>
        {configurationError ? (
          <Text label={configurationError} color={'danger'} wrap={true} />
        ) : null}
        {error ? <Text label={boundedMessage(error)} color={'danger'} wrap={true} /> : null}
        {notice ? <Text label={notice} color={'positive'} wrap={true} /> : null}
      </Card>
    </Expander>
  );
}
