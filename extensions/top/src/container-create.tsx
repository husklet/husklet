import React from 'react';
import {
  Button,
  Card,
  CardActions,
  CardContent,
  CardHeader,
  Column,
  Entry,
  Expander,
  FormControl,
  FormLabel,
  Heading,
  InlineMessage,
  RecoveryState,
  Row,
  Select,
  Spinner,
  Switch,
  TagInput,
  Text,
  type ContainerCreateSpec,
  type WorkspaceApi,
} from '@husklet/react';
import { boundedMessage } from './model.js';

const { useState } = React;

function EntryField({
  label,
  value,
  placeholder,
  enabled,
  width = { chars: 32 },
  helper,
  onChange,
}: {
  label: string;
  value: string;
  placeholder?: string;
  enabled: boolean;
  width?: { chars: number } | 'fill';
  helper?: string;
  onChange: (value: string) => void;
}) {
  return (
    <FormControl gap={1} width={width}>
      <FormLabel label={label} />
      <Entry
        value={value}
        placeholder={placeholder}
        enabled={enabled}
        width="fill"
        onChange={(event) => onChange(String(event.value ?? ''))}
      />
      {helper ? <Text label={helper} color="text-dim" wrap /> : null}
    </FormControl>
  );
}

function ArgumentEditor({
  label,
  value,
  enabled,
  onChange,
}: {
  label: string;
  value: string[];
  enabled: boolean;
  onChange: (value: string[]) => void;
}) {
  const [argument, setArgument] = useState('');
  const add = () => {
    if (!argument || value.length >= 64) return;
    onChange([...value, argument]);
    setArgument('');
  };
  return (
    <FormControl gap={1} width={{ chars: 28 }}>
      <FormLabel label={label} />
      <TagInput
        value={argument}
        placeholder={`Add ${label.toLowerCase()} argument`}
        enabled={enabled}
        onChange={(event) => setArgument(String(event.value ?? ''))}
        onSubmit={add}
      />
      <Row gap={1} wrap>
        {value.map((item, index) => (
          <Button
            key={`${index}:${item}`}
            label={`${index + 1} · ${item}`}
            tooltip={`Remove argument ${index + 1}`}
            variant="ghost"
            enabled={enabled}
            onInvoke={() => onChange(value.filter((_, held) => held !== index))}
          />
        ))}
      </Row>
    </FormControl>
  );
}

function EnvironmentEditor({
  value,
  enabled,
  onChange,
  label = 'Environment variables',
  namePlaceholder = 'Variable name',
  valuePlaceholder = 'Variable value',
  addLabel = 'Add variable',
}: {
  value: [string, string][];
  enabled: boolean;
  onChange: (value: [string, string][]) => void;
  label?: string;
  namePlaceholder?: string;
  valuePlaceholder?: string;
  addLabel?: string;
}) {
  const [name, setName] = useState('');
  const [entryValue, setEntryValue] = useState('');
  const add = () => {
    if (!name || name.includes('=') || value.some(([held]) => held === name)) return;
    onChange([...value, [name, entryValue]]);
    setName('');
    setEntryValue('');
  };
  return (
    <FormControl gap={1}>
      <FormLabel label={label} />
      <Row gap={1} wrap>
        <Entry
          value={name}
          placeholder={namePlaceholder}
          enabled={enabled}
          onChange={(event) => setName(String(event.value ?? ''))}
        />
        <Entry
          value={entryValue}
          placeholder={valuePlaceholder}
          enabled={enabled}
          onChange={(event) => setEntryValue(String(event.value ?? ''))}
        />
        <Button
          label={addLabel}
          size="small"
          variant="outline"
          enabled={enabled && Boolean(name) && !name.includes('=')}
          onInvoke={add}
        />
      </Row>
      <Row gap={1} wrap>
        {value.map(([heldName, heldValue], index) => (
          <Button
            key={`${heldName}:${index}`}
            label={`${heldName}=${heldValue}`}
            tooltip={`Remove ${heldName}`}
            variant="ghost"
            enabled={enabled}
            onInvoke={() => onChange(value.filter((_, held) => held !== index))}
          />
        ))}
      </Row>
    </FormControl>
  );
}

export type ContainerCreateDraft = {
  image: string;
  name: string;
  hostname: string;
  user: string;
  labels: [string, string][];
  network: string;
  entrypoint: string[];
  command: string[];
  environment: [string, string][];
  workingDirectory: string;
  memoryMb: string;
  cpus: string;
  pidsLimit: string;
  mounts: VolumeMount[];
  ports: PublishedPort[];
};

type CreatedContainer = { id: string; name: string; generation: number };

type ContainerCreateProps = {
  api: WorkspaceApi;
  blocked: boolean;
  label?: string;
  prominent?: boolean;
  onBusyChange: (busy: boolean) => void;
  reload: () => void | Promise<void>;
};

const emptyDraft = (): ContainerCreateDraft => ({
  image: '',
  name: '',
  hostname: '',
  user: '',
  labels: [],
  network: '',
  entrypoint: [],
  command: [],
  environment: [],
  workingDirectory: '',
  memoryMb: '',
  cpus: '',
  pidsLimit: '',
  mounts: [],
  ports: [],
});

const byteLength = (value: string) => new TextEncoder().encode(value).byteLength;

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

function MountEditor({
  value,
  enabled,
  onChange,
}: {
  value: VolumeMount[];
  enabled: boolean;
  onChange: (value: VolumeMount[]) => void;
}) {
  const [volume, setVolume] = useState('');
  const [target, setTarget] = useState('');
  const [readOnly, setReadOnly] = useState(false);
  const add = () => {
    if (!volume || !target || value.length >= 64) return;
    onChange([...value, { volume, target, read_only: readOnly }]);
    setVolume('');
    setTarget('');
    setReadOnly(false);
  };
  return (
    <FormControl gap={1}>
      <FormLabel label="Volume mounts" />
      <Row gap={1} wrap>
        <Entry
          value={volume}
          placeholder="Mount volume"
          enabled={enabled}
          onChange={(event) => setVolume(String(event.value ?? ''))}
        />
        <Entry
          value={target}
          placeholder="Container path"
          enabled={enabled}
          onChange={(event) => setTarget(String(event.value ?? ''))}
        />
        <FormControl gap={1}>
          <FormLabel label="Read only" />
          <Switch
            checked={readOnly}
            enabled={enabled}
            onToggle={(event) => setReadOnly(Boolean(event.value))}
          />
        </FormControl>
        <Button
          label="Add mount"
          size="small"
          variant="outline"
          enabled={enabled && Boolean(volume) && Boolean(target)}
          onInvoke={add}
        />
      </Row>
      <Row gap={1} wrap>
        {value.map((entry, index) => (
          <Button
            key={`${String(entry.volume)}:${index}`}
            label={`${String(entry.volume)} → ${String(entry.target)}${entry.read_only ? ' · read only' : ''}`}
            tooltip={`Remove mount ${index + 1}`}
            variant="ghost"
            enabled={enabled}
            onInvoke={() => onChange(value.filter((_, held) => held !== index))}
          />
        ))}
      </Row>
    </FormControl>
  );
}

function PortEditor({
  value,
  enabled,
  onChange,
}: {
  value: PublishedPort[];
  enabled: boolean;
  onChange: (value: PublishedPort[]) => void;
}) {
  const [containerPort, setContainerPort] = useState('');
  const [hostPort, setHostPort] = useState('');
  const [protocol, setProtocol] = useState('tcp');
  const add = () => {
    const container = Number(containerPort);
    const host = hostPort ? Number(hostPort) : null;
    if (!isPortNumber(container) || (host !== null && !isPortNumber(host)) || value.length >= 64)
      return;
    onChange([...value, { container, host, protocol: protocol as 'tcp' | 'udp' }]);
    setContainerPort('');
    setHostPort('');
  };
  const validDraft =
    isPortNumber(Number(containerPort)) && (!hostPort || isPortNumber(Number(hostPort)));
  return (
    <FormControl gap={1}>
      <FormLabel label="Published ports" />
      <Row gap={1} wrap>
        <Entry
          value={containerPort}
          placeholder="Container port"
          enabled={enabled}
          onChange={(event) => setContainerPort(String(event.value ?? ''))}
        />
        <Entry
          value={hostPort}
          placeholder="Host port (automatic if empty)"
          enabled={enabled}
          onChange={(event) => setHostPort(String(event.value ?? ''))}
        />
        <Select
          value={protocol}
          choices={[
            { value: 'tcp', label: 'TCP' },
            { value: 'udp', label: 'UDP' },
          ]}
          enabled={enabled}
          onChange={(event) => setProtocol(String(event.value ?? 'tcp'))}
        />
        <Button
          label="Publish port"
          size="small"
          variant="outline"
          enabled={enabled && validDraft}
          onInvoke={add}
        />
      </Row>
      <Row gap={1} wrap>
        {value.map((entry, index) => (
          <Button
            key={`${String(entry.container)}:${String(entry.protocol)}:${index}`}
            label={`${String(entry.host ?? 'auto')} → ${String(entry.container)}/${String(entry.protocol)}`}
            tooltip={`Remove published port ${index + 1}`}
            variant="ghost"
            enabled={enabled}
            onInvoke={() => onChange(value.filter((_, held) => held !== index))}
          />
        ))}
      </Row>
    </FormControl>
  );
}

export function parseLabels(value: [string, string][]): [string, string][] | undefined {
  if (value.length === 0) {
    return undefined;
  }
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

export function parseArguments(
  value: string[],
  kind: 'Command' | 'Entrypoint',
): string[] | undefined {
  if (value.length === 0) {
    return undefined;
  }
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

export function parseEnvironment(value: [string, string][]): [string, string][] | undefined {
  if (value.length === 0) {
    return undefined;
  }
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
  const labels = parseLabels(draft.labels);
  const network = draft.network;
  if (network && (byteLength(network) > 255 || !/^[A-Za-z0-9][A-Za-z0-9_.-]*$/.test(network))) {
    throw new Error(
      'Initial network must start with an ASCII letter or digit, contain only ASCII letters, digits, dots, underscores or hyphens, and be at most 255 bytes.',
    );
  }
  const entrypoint = parseArguments(draft.entrypoint, 'Entrypoint');
  const command = parseArguments(draft.command, 'Command');
  if (
    [...(entrypoint ?? []), ...(command ?? [])].reduce(
      (total, argument) => total + byteLength(argument),
      0,
    ) > 32_768
  ) {
    throw new Error('Entrypoint and command together must contain at most 32768 bytes.');
  }
  const environment = parseEnvironment(draft.environment);
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
  const mounts = parseMounts(draft.mounts);
  const ports = parsePorts(draft.ports);
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

export function parseMounts(mounts: VolumeMount[]): VolumeMount[] | undefined {
  if (mounts.length === 0) return undefined;
  {
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
    return (mounts as VolumeMount[]).map(({ volume, target, read_only = false }) => ({
      volume,
      target,
      read_only,
    }));
  }
}

export function parsePorts(ports: PublishedPort[]): PublishedPort[] | undefined {
  if (ports.length === 0) return undefined;
  {
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
    return (ports as PublishedPort[]).map(({ container, host = null, protocol }) => ({
      container,
      host,
      protocol,
    }));
  }
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

export function ContainerCreate({
  api,
  blocked,
  label = 'Create a container',
  onBusyChange,
  reload,
}: ContainerCreateProps) {
  const [expanded, setExpanded] = useState(false);
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
  const missingRequired = [
    draft.image.trim() ? '' : 'image',
    draft.name.trim() ? '' : 'name',
  ].filter(Boolean);
  const requirements = configurationError
    ? configurationError
    : missingRequired.length > 0
      ? `Add ${missingRequired.join(' and ')}.`
      : 'Ready to create and start.';
  const update = <K extends keyof ContainerCreateDraft>(
    field: K,
    value: ContainerCreateDraft[K],
  ) => {
    setDraft((current) => ({ ...current, [field]: value }));
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
    <Column gap={1}>
      <Row gap={1} justify="start">
        <Button
          label={label}
          size="small"
          variant="outline"
          tooltip={expanded ? 'Container setup is open' : 'Configure a new container'}
          onInvoke={() => setExpanded(true)}
        />
      </Row>
      {expanded ? (
        <Card variant="outline" width="fill">
          <CardHeader
            label="New container"
            detail="Choose an image and name. Husklet verifies creation before starting it."
            align="start"
            width="fill"
          />
          <CardContent gap={1}>
            <Heading label="Required" scale="body" />
            <Row gap={2} wrap={true}>
              <EntryField
                label="Image reference · required"
                value={draft.image}
                placeholder="Image reference"
                helper="For example alpine:3.20 or a locally imported image."
                enabled={editable}
                onChange={(value) => update('image', value)}
              />
              <EntryField
                label="Container name · required"
                value={draft.name}
                placeholder="Container name"
                helper="A stable name used by terminal and management extensions."
                enabled={editable}
                onChange={(value) => update('name', value)}
              />
            </Row>
            {configurationError ? <InlineMessage label={configurationError} tone="danger" /> : null}
            <Expander label="Identity and metadata" expanded={false} width="fill">
              <Column gap={1}>
                <Row gap={1} wrap>
                  <EntryField
                    label="Hostname"
                    value={draft.hostname}
                    placeholder={'Hostname (optional)'}
                    enabled={editable}
                    onChange={(value) => update('hostname', value)}
                  />
                  <EntryField
                    label="Run as user"
                    value={draft.user}
                    placeholder={'Run as user (optional)'}
                    enabled={editable}
                    onChange={(value) => update('user', value)}
                  />
                </Row>
                <EnvironmentEditor
                  value={draft.labels}
                  enabled={editable}
                  onChange={(value) => update('labels', value)}
                  label="Labels"
                  namePlaceholder="Label name"
                  valuePlaceholder="Label value"
                  addLabel="Add label"
                />
              </Column>
            </Expander>
            <Expander label="Process overrides" expanded={false}>
              <Column gap={1} width="fill">
                <Text
                  label="Optional. Entrypoint replaces the image program; Command supplies its arguments. Add each argument with Enter or Add."
                  color="text-dim"
                  wrap
                />
                <Row gap={1} wrap={true}>
                  <ArgumentEditor
                    label="Entrypoint"
                    value={draft.entrypoint}
                    enabled={editable}
                    onChange={(value) => update('entrypoint', value)}
                  />
                  <ArgumentEditor
                    label="Command"
                    value={draft.command}
                    enabled={editable}
                    onChange={(value) => update('command', value)}
                  />
                </Row>
                <EntryField
                  label="Working directory"
                  value={draft.workingDirectory}
                  placeholder={'Working directory (optional)'}
                  enabled={editable}
                  width={{ chars: 36 }}
                  onChange={(value) => update('workingDirectory', value)}
                />
                <EnvironmentEditor
                  value={draft.environment}
                  enabled={editable}
                  onChange={(value) => update('environment', value)}
                />
              </Column>
            </Expander>
            <Expander label="Resources and networking" expanded={false} width="fill">
              <Column gap={1}>
                <Row gap={1} wrap={true}>
                  <EntryField
                    label="Memory limit"
                    value={draft.memoryMb}
                    placeholder={'Memory limit MiB (optional)'}
                    enabled={editable}
                    onChange={(value) => update('memoryMb', value)}
                  />
                  <EntryField
                    label="CPU limit"
                    value={draft.cpus}
                    placeholder={'CPU limit (optional)'}
                    enabled={editable}
                    onChange={(value) => update('cpus', value)}
                  />
                  <EntryField
                    label="PID limit"
                    value={draft.pidsLimit}
                    placeholder={'PID limit (optional)'}
                    enabled={editable}
                    onChange={(value) => update('pidsLimit', value)}
                  />
                  <EntryField
                    label="Initial network"
                    value={draft.network}
                    placeholder={'Initial network (optional)'}
                    enabled={editable}
                    onChange={(value) => update('network', value)}
                  />
                </Row>
                <MountEditor
                  value={draft.mounts}
                  enabled={editable}
                  onChange={(value) => update('mounts', value)}
                />
                <PortEditor
                  value={draft.ports}
                  enabled={editable}
                  onChange={(value) => update('ports', value)}
                />
                <Text
                  label="Mounts accept named volumes only. Published host ports may be left automatic."
                  color="text-dim"
                  wrap
                />
              </Column>
            </Expander>
            {!configurationError ? (
              <InlineMessage
                label={
                  created ? `Created ${created.name}; start can be retried safely.` : requirements
                }
                tone={missingRequired.length > 0 ? 'neutral' : 'positive'}
              />
            ) : null}
          </CardContent>
          <CardActions gap={1} align="center" justify="start" width="fill">
            {blocked ? <Spinner /> : null}
            <Button
              label={created ? 'Retry start' : blocked ? 'Creating…' : 'Create and start'}
              size="small"
              enabled={
                !blocked &&
                (created !== null || (missingRequired.length === 0 && !configurationError))
              }
              onInvoke={createAndStart}
            />
            {!created && !blocked ? (
              <Button
                label="Cancel"
                size="small"
                variant="ghost"
                onInvoke={() => {
                  setExpanded(false);
                  setError(null);
                }}
              />
            ) : null}
          </CardActions>
          {error ? (
            <CardContent gap={1}>
              <RecoveryState
                operation={created ? 'Starting container' : 'Creating container'}
                error={error}
              />
            </CardContent>
          ) : null}
          {notice ? (
            <CardContent gap={1}>
              <InlineMessage label={notice} tone="positive" />
            </CardContent>
          ) : null}
        </Card>
      ) : null}
    </Column>
  );
}
