import React from 'react';
import {
  Accordion,
  AccordionDetails,
  AccordionSummary,
  Button,
  Card,
  CardActions,
  CardContent,
  ColorPicker,
  Column,
  Entry,
  FormControlLabel,
  Heading,
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
const CONTROL_WIDTH = 'fill' as const;

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
      setError(message(cause));
    } finally {
      setSaving(false);
    }
  };
  if (!configuration)
    return (
      <Column pad={2} gap={2}>
        <Heading label="Workspace" scale="title" />
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
    <Scroll grow height="fill">
      <Column pad={2} gap={2}>
        <Card grow={false} justify="start" width="fill" variant="outline">
          <CardContent gap={2}>
            <Heading label="Workspace" scale="title" />
            <Text
              label="Settings save without stopping your workspace. Runtime identity changes apply when the workspace or panes reopen."
              color="text-dim"
              width={CONTROL_WIDTH}
              wrap
            />
            <Text
              label={dirty ? 'Unsaved changes' : saved ? 'Saved' : 'No changes'}
              color={dirty ? 'warning' : saved ? 'positive' : 'text-dim'}
            />
            <Row gap={1} wrap justify="start" width="fill">
              <Button
                variant="filled"
                tone="accent"
                label={saving ? 'Saving…' : 'Save workspace'}
                enabled={!saving && dirty && !invalid}
                onInvoke={save}
              />
              <Button
                label="Discard changes"
                variant="ghost"
                enabled={!saving && dirty}
                onInvoke={load}
              />
            </Row>
            {invalid && <InlineMessage label={invalid} tone="danger" />}
            {error && <RecoveryState operation="Saving workspace settings" error={error} />}
            {saved && <InlineMessage label={saved} tone="positive" />}
            <SettingsGroup
              name="runtime"
              label="Runtime"
              detail={`linux/${configuration.architecture} · ${configuration.name}`}
              expanded={expanded}
              onExpand={setExpanded}
            >
              {field('Workspace image', configuration.image, 'registry/image:tag', (event) =>
                change('image', String(event.value ?? '').trim()),
              )}
              {field(
                'Storage directory',
                configuration.storage ?? '',
                'Husklet-managed when empty',
                (event) => change('storage', nullable(event.value)),
              )}
              <Text
                label="Changing storage is refused while this workspace is running; other runtime settings are saved for the next restart."
                color="text-dim"
                width={CONTROL_WIDTH}
                wrap
              />
              {field('Default shell', configuration.shell ?? '', 'Automatic when empty', (event) =>
                change('shell', nullable(event.value)),
              )}
              {field('CPU limit', numbers.cpus, 'CPU count or empty', (event) =>
                numeric('cpus', event.value),
              )}
              {field('Memory (MB)', numbers.memory, 'Memory limit or empty', (event) =>
                numeric('memory', event.value),
              )}
              {field('Scrollback lines', numbers.scrollback, 'Scrollback limit or empty', (event) =>
                numeric('scrollback', event.value),
              )}
              <Column gap={1}>
                <Text label="Execution lifetime" />
                <Select
                  width={CONTROL_WIDTH}
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
              </Column>
              {field(
                'VPN proxy',
                configuration.vpn ?? '',
                'socks5://host:port (optional)',
                (event) => change('vpn', nullable(event.value)),
              )}
              <Row gap={2} align="center">
                <Switch
                  checked={configuration.docker_socket}
                  onToggle={(event: Change) => change('docker_socket', Boolean(event.value))}
                />
                <Text label="Expose Docker-compatible workspace socket" />
              </Row>
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
              <Column gap={1}>
                <Text label="Cursor shape" />
                <Select
                  value={configuration.terminal.cursor_shape ?? ''}
                  choices={[
                    { value: '', label: 'Host default' },
                    { value: 'block', label: 'Block' },
                    { value: 'ibeam', label: 'I-beam' },
                    { value: 'underline', label: 'Underline' },
                  ]}
                  onChange={(event: Change) => terminal('cursor_shape', nullable(event.value))}
                />
              </Column>
              <Row gap={2} align="center">
                <Switch
                  checked={configuration.terminal.cursor_blink ?? false}
                  onToggle={(event: Change) => terminal('cursor_blink', Boolean(event.value))}
                />
                <Text label="Cursor blink" />
                <Button
                  label="Use host default for cursor blink"
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
              detail={`${configuration.environment.length} configured`}
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
              detail={`${configuration.mounts.length} configured`}
              expanded={expanded}
              onExpand={setExpanded}
            >
              <Mounts values={configuration.mounts} onChange={(value) => change('mounts', value)} />
            </SettingsGroup>
          </CardContent>
        </Card>
      </Column>
    </Scroll>
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
      <AccordionSummary label={label}>
        <Text label={detail} color="text-dim" wrap />
      </AccordionSummary>
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
        <Row key={`${index}:${row[0]}`} gap={1}>
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
          <Button
            label={`Remove ${row[0] || `variable ${index + 1}`}`}
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
          <Row gap={1}>
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
          <Row gap={2} align="center">
            <Switch
              checked={mount.read_only}
              onToggle={(event: Change) => replace(index, { read_only: Boolean(event.value) })}
            />
            <Text label="Read only" />
            <Button
              label={`Remove mount ${index + 1}`}
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
    <Column gap={1}>
      <Text label={label} />
      <Entry
        value={value}
        placeholder={placeholder}
        grow={false}
        width={CONTROL_WIDTH}
        onChange={onChange}
      />
    </Column>
  );
}
function colorField(label: string, value: string | null, onChange: (value: string | null) => void) {
  return (
    <Column gap={1}>
      <Text label={label} />
      <Row gap={1} align="center">
        <ColorPicker
          value={value ?? '#000000'}
          onChange={(event: Change) => onChange(nullable(event.value))}
        />
        <Button
          label={`Use host default for ${label.toLowerCase()}`}
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
  return `${value.terminal.font_family ?? 'Host font'} · ${value.terminal.font_size ?? 'default size'} · ${value.terminal.cursor_shape ?? 'default cursor'}`;
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
