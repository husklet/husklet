import React from 'react';
import {
  Accordion,
  AccordionDetails,
  AccordionSummary,
  Badge,
  Button,
  Card,
  CardActions,
  CardContent,
  ColorPicker,
  Column,
  ConfirmAction,
  Container,
  Entry,
  FormControl,
  FormLabel,
  FormControlLabel,
  Heading,
  IconButton,
  InlineMessage,
  RecoveryState,
  Row,
  Scroll,
  Select,
  Spinner,
  Switch,
  Text,
  type WorkspaceApi,
  type WorkspaceConfiguration,
  type WorkspaceMount,
} from '@husklet/react';

type Change = { value?: unknown; expanded?: boolean };
type Numbers = { cpus: string; memory: string; scrollback: string; fontSize: string };
const CONTROL_WIDTH = { chars: 56 } as const;
const PAGE_WIDTH = { maximum: { chars: 110 } } as const;

export function Workspace({ api }: { api: WorkspaceApi }) {
  const [configuration, setConfiguration] = React.useState<WorkspaceConfiguration | null>(null);
  const [observed, setObserved] = React.useState<WorkspaceConfiguration | null>(null);
  const [numbers, setNumbers] = React.useState<Numbers>({
    cpus: '',
    memory: '',
    scrollback: '',
    fontSize: '',
  });
  const [error, setError] = React.useState('');
  const [saved, setSaved] = React.useState('');
  const [saving, setSaving] = React.useState(false);
  const [hydrated, setHydrated] = React.useState(false);
  const [expanded, setExpanded] = React.useState('runtime');
  const load = React.useCallback(async () => {
    setHydrated(false);
    try {
      const current = await api.info();
      const inspected = await api.inspect(current.name);
      setConfiguration(inspected);
      setObserved(inspected);
      setNumbers(numberDraft(inspected));
      setError('');
      setSaved('');
      setHydrated(true);
    } catch (cause) {
      setError(message(cause));
    }
  }, [api]);
  React.useEffect(() => {
    void load();
  }, [load]);
  const changed = () => {
    setSaved('');
  };
  const change = <K extends keyof WorkspaceConfiguration>(
    key: K,
    value: WorkspaceConfiguration[K],
  ) => {
    setConfiguration((current) => current && { ...current, [key]: value });
    changed();
  };
  const terminal = <K extends keyof WorkspaceConfiguration['terminal']>(
    key: K,
    value: WorkspaceConfiguration['terminal'][K],
  ) => {
    setConfiguration(
      (current) => current && { ...current, terminal: { ...current.terminal, [key]: value } },
    );
    changed();
  };
  const numeric = (key: keyof Numbers, value: unknown) => {
    setNumbers((current) => ({ ...current, [key]: String(value ?? '') }));
    changed();
  };
  const save = async () => {
    if (!configuration || !observed || saving) return;
    if (!configuration.generation || !configuration.configuration_revision) {
      setError('The host did not provide workspace revision identity; reload before saving.');
      return;
    }
    setSaving(true);
    try {
      const candidate = withNumbers(configuration, numbers);
      validate(candidate);
      const environmentPatch = diffEnvironment(observed.environment, candidate.environment);
      const settings = { ...candidate, environment: observed.environment };
      let updated = await api.update(
        configuration.name,
        configuration.generation,
        configuration.configuration_revision,
        settings,
      );
      if (environmentPatch.set.length || environmentPatch.remove.length) {
        if (!updated.configuration_revision) {
          throw new Error(
            'The host did not return the saved workspace revision; reload before changing environment values.',
          );
        }
        try {
          const result = await api.patchEnvironment(
            updated.name,
            updated.generation ?? configuration.generation,
            updated.configuration_revision,
            environmentPatch,
          );
          updated = { ...updated, ...result, environment: candidate.environment };
        } catch (cause) {
          const patchError = message(cause);
          try {
            const current = await api.info();
            const inspected = await api.inspect(current.name);
            setConfiguration({ ...inspected, environment: candidate.environment });
            setObserved(inspected);
            setNumbers(numberDraft(inspected));
            setSaved('');
            setError(
              `Settings were saved, but environment changes were not. Reloaded the latest workspace and retained your concealed environment edits for review and retry: ${patchError}`,
            );
          } catch (reloadCause) {
            setError(
              `Settings were saved, but environment changes were not (${patchError}); reload also failed: ${message(reloadCause)}`,
            );
          }
          return;
        }
      }
      setConfiguration(updated);
      setObserved(updated);
      setNumbers(numberDraft(updated));
      setError('');
      setSaved(
        'Workspace settings saved. Reopen panes or restart the workspace for runtime changes.',
      );
    } catch (cause) {
      setError(`No successful save was confirmed. Your edits are retained: ${message(cause)}`);
    } finally {
      setSaving(false);
    }
  };
  if (!configuration)
    return (
      <Column width="fill" pad={4} gap={3}>
        <Heading label="Workspace settings" scale="title" />
        {error ? (
          <RecoveryState
            operation="Workspace settings"
            error={error}
            retryLabel="Retry"
            onRetry={load}
          />
        ) : (
          <Row gap={2}>
            <Spinner />
            <Text label="Loading workspace settings…" />
          </Row>
        )}
      </Column>
    );
  const invalid = validationMessage(configuration, numbers);
  const dirty = hydrated && changedFrom(configuration, observed, numbers);
  return (
    <Column width="fill" height="fill">
      <Container
        grow={false}
        width={PAGE_WIDTH}
        pad={{ top: 4, end: 4, bottom: 0, start: 4 }}
        gap={1}
      >
        <Row gap={2} wrap justify="start" align="center" width="fill">
          <Column gap={0} grow>
            <Heading label="Workspace settings" scale="title" />
            <Text
              label={`linux/${configuration.architecture} · ${configuration.name}`}
              color="text-dim"
            />
          </Column>
          <Row gap={1} wrap justify="start" align="center">
            {dirty || saving ? (
              <>
                <Button
                  size="small"
                  variant="filled"
                  tone="accent"
                  label={saving ? 'Saving…' : 'Save changes'}
                  enabled={!saving && dirty && !invalid}
                  onInvoke={save}
                />
                <Button
                  size="small"
                  label="Discard"
                  variant="ghost"
                  enabled={!saving}
                  onInvoke={load}
                />
              </>
            ) : (
              <Badge label={saved ? 'Saved' : 'Up to date'} tone={saved ? 'positive' : 'neutral'} />
            )}
          </Row>
        </Row>
        <Text
          label={
            invalid
              ? 'Fix the highlighted settings before saving.'
              : dirty
                ? 'Unsaved changes'
                : saved
                  ? 'Changes saved'
                  : 'Changes save without stopping the workspace.'
          }
          color={invalid ? 'danger' : dirty ? 'warning' : saved ? 'positive' : 'text-dim'}
          wrap
        />
        {invalid && <InlineMessage label={invalid} tone="danger" />}
        {error && (
          <RecoveryState
            operation="Saving workspace settings"
            error={error}
            retryLabel="Retry save"
            onRetry={dirty && !invalid ? save : undefined}
          />
        )}
        {saved && <InlineMessage label={saved} tone="positive" />}
      </Container>
      <Scroll grow width="fill" height="fill">
        <Container pad={4} gap={3} width={PAGE_WIDTH}>
          <Card grow={false} width="fill" variant="plain">
            <CardContent gap={2}>
              <Text
                label="Choose a section. Only one stays open, so the setting you need remains easy to find."
                color="text-dim"
                width={CONTROL_WIDTH}
                wrap
              />
              <SettingsGroup
                name="runtime"
                label="Runtime"
                detail={`Image ${configuration.image} · Shell ${configuration.shell ?? 'automatic'}`}
                expanded={expanded}
                onExpand={setExpanded}
              >
                {field('Workspace image', configuration.image, 'registry/image:tag', (event) =>
                  change('image', String(event.value ?? '').trim()),
                )}
                <Text
                  label="Image and shell changes apply when the workspace or panes reopen."
                  color="text-dim"
                  width={CONTROL_WIDTH}
                  wrap
                />
                {field(
                  'Default shell',
                  configuration.shell ?? '',
                  'Automatic when empty',
                  (event) => change('shell', nullable(event.value)),
                )}
                <FormControl gap={1}>
                  <FormLabel label="Execution lifetime" />
                  <Select
                    width={CONTROL_WIDTH}
                    align="start"
                    value={configuration.execution_lifetime}
                    choices={[
                      { value: 'persisted', label: 'Persisted across restarts' },
                      { value: 'live', label: 'Live until shutdown' },
                      { value: 'ephemeral', label: 'Ephemeral per execution' },
                    ]}
                    onChange={(event: Change) =>
                      change(
                        'execution_lifetime',
                        String(
                          event.value ?? 'persisted',
                        ) as WorkspaceConfiguration['execution_lifetime'],
                      )
                    }
                  />
                </FormControl>
                <Column gap={1} width="fill">
                  <Heading label="Runtime access" scale="caption" />
                  <Text
                    label="Docker-compatible socket access lets processes in this workspace control its container engine. Only enable it for trusted workspace code."
                    color="text-dim"
                    wrap
                  />
                  <Text
                    label="This change takes effect after the workspace restarts."
                    color="text-dim"
                    wrap
                  />
                  {configuration.docker_socket ? (
                    <Column gap={1}>
                      <InlineMessage
                        label="Docker-compatible workspace socket is enabled."
                        tone="warning"
                      />
                      <Button
                        label="Disable Docker socket"
                        size="small"
                        variant="outline"
                        onInvoke={() => change('docker_socket', false)}
                      />
                    </Column>
                  ) : (
                    <ConfirmAction
                      authorityKey={`docker-socket:${configuration.generation}:${configuration.configuration_revision}`}
                      label="Enable Docker socket"
                      size="small"
                      confirmLabel="Confirm socket access"
                      pendingLabel="Enabling…"
                      question="Allow trusted workspace processes to control this workspace’s container engine after restart?"
                      onConfirm={() => change('docker_socket', true)}
                    />
                  )}
                </Column>
              </SettingsGroup>
              <SettingsGroup
                name="advanced"
                label="Resources & connectivity"
                detail={resourceSummary(configuration)}
                expanded={expanded}
                onExpand={setExpanded}
              >
                {field('CPU limit', numbers.cpus, 'CPU count or empty', (event) =>
                  numeric('cpus', event.value),
                )}
                {field('Memory (MB)', numbers.memory, 'Memory limit or empty', (event) =>
                  numeric('memory', event.value),
                )}
                {field(
                  'Scrollback lines',
                  numbers.scrollback,
                  'Scrollback limit or empty',
                  (event) => numeric('scrollback', event.value),
                )}
                {field(
                  'VPN proxy',
                  configuration.vpn ?? '',
                  'socks5://host:port (optional)',
                  (event) => change('vpn', nullable(event.value)),
                )}
                {field(
                  'Storage directory',
                  configuration.storage ?? '',
                  'Husklet-managed when empty',
                  (event) => change('storage', nullable(event.value)),
                )}
                <Text
                  label="Storage cannot change while the workspace is running. Resource and proxy changes apply after restart."
                  color="text-dim"
                  width={CONTROL_WIDTH}
                  wrap
                />
              </SettingsGroup>
              <SettingsGroup
                name="terminal"
                label="Terminal appearance"
                detail={terminalSummary(configuration)}
                expanded={expanded}
                onExpand={setExpanded}
              >
                {field(
                  'Font family',
                  configuration.terminal.font_family ?? '',
                  'Host default',
                  (event) => terminal('font_family', nullable(event.value)),
                )}
                {field('Font size', numbers.fontSize, 'Host default', (event) =>
                  numeric('fontSize', event.value),
                )}
                {colorField('Foreground', configuration.terminal.foreground, (value) =>
                  terminal('foreground', value),
                )}
                {colorField('Background', configuration.terminal.background, (value) =>
                  terminal('background', value),
                )}
                <FormControl gap={1}>
                  <FormLabel label="Cursor shape" />
                  <Select
                    width={CONTROL_WIDTH}
                    align="start"
                    value={configuration.terminal.cursor_shape ?? ''}
                    choices={[
                      { value: '', label: 'Host default' },
                      { value: 'block', label: 'Block' },
                      { value: 'ibeam', label: 'I-beam' },
                      { value: 'underline', label: 'Underline' },
                    ]}
                    onChange={(event: Change) => terminal('cursor_shape', nullable(event.value))}
                  />
                </FormControl>
                <Row gap={1} align="center" wrap>
                  <FormControlLabel label="Cursor blink" gap={2}>
                    <Switch
                      checked={configuration.terminal.cursor_blink ?? false}
                      onToggle={(event: Change) => terminal('cursor_blink', Boolean(event.value))}
                    />
                  </FormControlLabel>
                  <IconButton
                    icon="edit-clear-symbolic"
                    label="Reset cursor blink"
                    tooltip="Use the host default for cursor blink"
                    variant="ghost"
                    enabled={configuration.terminal.cursor_blink !== null}
                    onInvoke={() => terminal('cursor_blink', null)}
                  />
                </Row>
                {configuration.terminal.cursor_blink === null ? (
                  <Text label="Cursor blink uses the host default." color="text-dim" />
                ) : null}
              </SettingsGroup>
              <SettingsGroup
                name="environment"
                label="Environment variables"
                detail={`${configuration.environment.length} ${configuration.environment.length === 1 ? 'variable' : 'variables'}`}
                expanded={expanded}
                onExpand={setExpanded}
              >
                <Environment
                  key={configuration.configuration_revision}
                  values={configuration.environment}
                  onChange={(value) => change('environment', value)}
                />
              </SettingsGroup>
              <SettingsGroup
                name="mounts"
                label="Filesystem mounts"
                detail={`${configuration.mounts.length} ${configuration.mounts.length === 1 ? 'mount' : 'mounts'}`}
                expanded={expanded}
                onExpand={setExpanded}
              >
                <Mounts
                  values={configuration.mounts}
                  onChange={(value) => change('mounts', value)}
                />
              </SettingsGroup>
            </CardContent>
          </Card>
        </Container>
      </Scroll>
    </Column>
  );
}

function changedFrom(
  configuration: WorkspaceConfiguration,
  observed: WorkspaceConfiguration | null,
  numbers: Numbers,
) {
  if (!observed) return false;
  return (
    JSON.stringify(configuration) !== JSON.stringify(observed) ||
    JSON.stringify(numbers) !== JSON.stringify(numberDraft(observed))
  );
}

function diffEnvironment(before: [string, string][], after: [string, string][]) {
  const previous = new Map(before);
  const next = new Map(after);
  return {
    set: after.filter(([name, value]) => previous.get(name) !== value),
    remove: before.map(([name]) => name).filter((name) => !next.has(name)),
  };
}

function SettingsGroup({
  name,
  label,
  detail,
  expanded,
  onExpand,
  children,
}: {
  name: string;
  label: string;
  detail: string;
  expanded: string;
  onExpand: (value: string) => void;
  children: React.ReactNode;
}) {
  const open = expanded === name;
  return (
    <Accordion
      label={label}
      expanded={open}
      onExpand={(event: Change) => onExpand((event.expanded ?? event.value) ? name : '')}
    >
      <AccordionSummary label={`${label} · ${detail}`} />
      <AccordionDetails gap={2}>{children}</AccordionDetails>
    </Accordion>
  );
}

function Environment({
  values,
  onChange,
}: {
  values: [string, string][];
  onChange: (value: [string, string][]) => void;
}) {
  const [revealed, setRevealed] = React.useState(false);
  const replace = (index: number, part: 0 | 1, value: unknown) =>
    onChange(
      values.map((row, at) =>
        at === index
          ? [part === 0 ? String(value ?? '') : row[0], part === 1 ? String(value ?? '') : row[1]]
          : row,
      ),
    );
  return (
    <Column gap={2}>
      {values.length > 0 && (
        <FormControlLabel label="Show environment values" gap={2}>
          <Switch
            checked={revealed}
            onToggle={(event: Change) => setRevealed(Boolean(event.value))}
          />
        </FormControlLabel>
      )}
      {values.map((row, index) => (
        <Row key={`${index}:${row[0]}`} gap={1} align="center" wrap>
          <Entry
            value={row[0]}
            placeholder="NAME"
            onChange={(event: Change) => replace(index, 0, event.value)}
          />
          <Entry
            value={row[1]}
            placeholder="value"
            secret={!revealed}
            grow
            onChange={(event: Change) => replace(index, 1, event.value)}
          />
          <IconButton
            icon="user-trash-symbolic"
            label={`Remove ${row[0] || `variable ${index + 1}`}`}
            tooltip={`Remove ${row[0] || `variable ${index + 1}`}`}
            variant="ghost"
            tone="danger"
            onInvoke={() => onChange(values.filter((_, at) => at !== index))}
          />
        </Row>
      ))}
      <CardActions>
        <Button label="Add variable" onInvoke={() => onChange([...values, ['', '']])} />
      </CardActions>
    </Column>
  );
}
function Mounts({
  values,
  onChange,
}: {
  values: WorkspaceMount[];
  onChange: (value: WorkspaceMount[]) => void;
}) {
  const replace = (index: number, patch: Partial<WorkspaceMount>) =>
    onChange(values.map((row, at) => (at === index ? { ...row, ...patch } : row)));
  return (
    <Column gap={2}>
      {values.map((mount, index) => (
        <Column key={`${index}:${mount.container}`} gap={1}>
          <Row gap={1} wrap>
            <Entry
              value={mount.host}
              placeholder="Host path"
              grow
              onChange={(event: Change) => replace(index, { host: String(event.value ?? '') })}
            />
            <Entry
              value={mount.container}
              placeholder="Absolute container path"
              grow
              onChange={(event: Change) => replace(index, { container: String(event.value ?? '') })}
            />
          </Row>
          <Row gap={1} align="center" wrap>
            <FormControlLabel label="Read only" gap={2}>
              <Switch
                checked={mount.read_only}
                onToggle={(event: Change) => replace(index, { read_only: Boolean(event.value) })}
              />
            </FormControlLabel>
            <IconButton
              icon="user-trash-symbolic"
              label={`Remove mount ${index + 1}`}
              tooltip={`Remove mount ${index + 1}`}
              variant="ghost"
              tone="danger"
              onInvoke={() => onChange(values.filter((_, at) => at !== index))}
            />
          </Row>
        </Column>
      ))}
      <CardActions>
        <Button
          label="Add mount"
          onInvoke={() => onChange([...values, { host: '', container: '', read_only: true }])}
        />
      </CardActions>
    </Column>
  );
}
function field(
  label: string,
  value: string,
  placeholder: string,
  onChange: (event: Change) => void,
) {
  return (
    <FormControl gap={1} width={CONTROL_WIDTH}>
      <FormLabel label={label} />
      <Entry
        value={value}
        placeholder={placeholder}
        grow={false}
        width={CONTROL_WIDTH}
        align="start"
        onChange={onChange}
      />
    </FormControl>
  );
}
function colorField(label: string, value: string | null, onChange: (value: string | null) => void) {
  return (
    <Column gap={1}>
      <Text label={label} />
      <Row gap={1} align="center" wrap>
        <ColorPicker
          value={value ?? '#000000'}
          onChange={(event: Change) => onChange(nullable(event.value))}
        />
        <IconButton
          icon="edit-clear-symbolic"
          label={`Reset ${label.toLowerCase()}`}
          tooltip={`Use the host default for ${label.toLowerCase()}`}
          variant="ghost"
          enabled={value !== null}
          onInvoke={() => onChange(null)}
        />
      </Row>
      {value === null && <Text label="Host default" color="text-dim" />}
    </Column>
  );
}
function nullable(value: unknown): string | null {
  const result = String(value ?? '').trim();
  return result || null;
}
function numberDraft(value: WorkspaceConfiguration): Numbers {
  return {
    cpus: text(value.cpus),
    memory: text(value.memory_mb),
    scrollback: text(value.scrollback),
    fontSize: text(value.terminal.font_size),
  };
}
function text(value: number | null): string {
  return value === null ? '' : String(value);
}
function terminalSummary(value: WorkspaceConfiguration): string {
  return `Font ${value.terminal.font_family ?? 'host default'} · Size ${value.terminal.font_size ?? 'default'} · Cursor ${value.terminal.cursor_shape ?? 'default'}`;
}
function resourceSummary(value: WorkspaceConfiguration): string {
  const cpu = value.cpus === null ? 'automatic' : String(value.cpus);
  const memory = value.memory_mb === null ? 'automatic' : `${value.memory_mb} MB`;
  return `CPU ${cpu} · Memory ${memory}`;
}
function optionalInteger(value: string, label: string, maximum: number): number | null {
  if (!value.trim()) return null;
  const parsed = Number(value);
  if (!Number.isSafeInteger(parsed) || parsed <= 0 || parsed > maximum)
    throw new Error(`${label} must be a whole number from 1 to ${maximum}, or empty.`);
  return parsed;
}
function withNumbers(value: WorkspaceConfiguration, numbers: Numbers): WorkspaceConfiguration {
  return {
    ...value,
    cpus: optionalInteger(numbers.cpus, 'CPU limit', 4_294_967_295),
    memory_mb: optionalInteger(numbers.memory, 'Memory', 4_294_967_295),
    scrollback: optionalInteger(numbers.scrollback, 'Scrollback', Number.MAX_SAFE_INTEGER),
    terminal: {
      ...value.terminal,
      font_size: optionalInteger(numbers.fontSize, 'Font size', 65_535),
    },
  };
}
function validate(value: WorkspaceConfiguration) {
  if (!value.image.trim()) throw new Error('Workspace image must not be empty.');
  for (const [label, color] of [
    ['Foreground', value.terminal.foreground],
    ['Background', value.terminal.background],
  ] as const)
    if (color && !/^#[0-9a-fA-F]{6}$/.test(color)) throw new Error(`${label} must use #RRGGBB.`);
  const names = new Set<string>();
  for (const [name] of value.environment) {
    if (!/^[A-Za-z_][A-Za-z0-9_]*$/.test(name))
      throw new Error(
        'Environment names must use letters, digits and underscores and cannot start with a digit.',
      );
    if (names.has(name)) throw new Error(`Environment variable ${name} is duplicated.`);
    names.add(name);
  }
  const targets = new Set<string>();
  for (const mount of value.mounts) {
    if (!mount.host.trim() || !normalizedMountTarget(mount.container))
      throw new Error('Every mount needs a host path and a normalized absolute container path.');
    if (targets.has(mount.container))
      throw new Error(`Mount target ${mount.container} is duplicated.`);
    targets.add(mount.container);
  }
}
function normalizedMountTarget(path: string): boolean {
  if (path === '/') return true;
  if (!path.startsWith('/')) return false;
  const components = path.slice(1).split('/');
  return (
    components.length > 0 &&
    components.every(
      (component) =>
        component !== '' && component !== '.' && component !== '..' && !component.includes('\0'),
    )
  );
}
function validationMessage(configuration: WorkspaceConfiguration, numbers: Numbers): string {
  try {
    validate(withNumbers(configuration, numbers));
    return '';
  } catch (cause) {
    return message(cause);
  }
}
function message(cause: unknown): string {
  return cause instanceof Error ? cause.message.slice(0, 500) : String(cause).slice(0, 500);
}
