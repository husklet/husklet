import React from 'react';
import {
  Accordion, AccordionDetails, AccordionSummary, Button, CardActions, ColorPicker, Column, Entry, Heading,
  InlineMessage, Row, Scroll, Select, Spinner, Switch, Text,
  type WorkspaceApi, type WorkspaceConfiguration, type WorkspaceMount,
} from '@husklet/react';

type Change = { value?: unknown; expanded?: boolean };
type Numbers = { cpus: string; memory: string; scrollback: string; fontSize: string };

export function Workspace({ api }: { api: WorkspaceApi }) {
  const [configuration, setConfiguration] = React.useState<WorkspaceConfiguration | null>(null);
  const [numbers, setNumbers] = React.useState<Numbers>({ cpus: '', memory: '', scrollback: '', fontSize: '' });
  const [error, setError] = React.useState('');
  const [saved, setSaved] = React.useState('');
  const [saving, setSaving] = React.useState(false);
  const [dirty, setDirty] = React.useState(false);
  const [expanded, setExpanded] = React.useState('runtime');
  const load = React.useCallback(async () => {
    try {
      const current = await api.info();
      const inspected = await api.inspect(current.name);
      setConfiguration(inspected); setNumbers(numberDraft(inspected));
      setError(''); setSaved(''); setDirty(false);
    } catch (cause) { setError(message(cause)); }
  }, [api]);
  React.useEffect(() => { void load(); }, [load]);
  const changed = () => { setSaved(''); setDirty(true); };
  const change = <K extends keyof WorkspaceConfiguration>(key: K, value: WorkspaceConfiguration[K]) => {
    setConfiguration((current) => current && ({ ...current, [key]: value })); changed();
  };
  const terminal = <K extends keyof WorkspaceConfiguration['terminal']>(key: K, value: WorkspaceConfiguration['terminal'][K]) => {
    setConfiguration((current) => current && ({ ...current, terminal: { ...current.terminal, [key]: value } })); changed();
  };
  const numeric = (key: keyof Numbers, value: unknown) => { setNumbers((current) => ({ ...current, [key]: String(value ?? '') })); changed(); };
  const save = async () => {
    if (!configuration || saving) return;
    if (!configuration.generation) { setError('The host did not provide a workspace generation; reload before saving.'); return; }
    setSaving(true);
    try {
      const candidate = withNumbers(configuration, numbers); validate(candidate);
      const updated = await api.update(configuration.name, configuration.generation, candidate);
      setConfiguration(updated); setNumbers(numberDraft(updated)); setDirty(false); setError('');
      setSaved('Workspace settings saved. Reopen panes or restart the workspace for runtime changes.');
    } catch (cause) { setError(message(cause)); } finally { setSaving(false); }
  };
  if (!configuration) return <Column pad={4} gap={2}>
    <Heading label="Workspace" scale="title" />
    {error ? <InlineMessage label={error} tone="danger" /> : <Row gap={2}><Spinner /><Text label="Loading workspace settings…" /></Row>}
    <Button label="Retry" enabled={Boolean(error)} onInvoke={load} />
  </Column>;
  const invalid = validationMessage(configuration, numbers);
  return <Scroll grow height="fill"><Column pad={4} gap={3}>
    <Heading label="Workspace" scale="title" />
    <Text label="Settings save without stopping your workspace. Runtime identity changes apply when the workspace or panes reopen." color="text-dim" wrap />
    <SettingsGroup name="runtime" label="Runtime" detail={`linux/${configuration.architecture} · ${configuration.name}`} expanded={expanded} onExpand={setExpanded}>
      {field('Workspace image', configuration.image, 'registry/image:tag', (event) => change('image', String(event.value ?? '').trim()))}
      {field('Storage directory', configuration.storage ?? '', 'Husklet-managed when empty', (event) => change('storage', nullable(event.value)))}
      <Text label="Changing storage is refused while this workspace is running; other runtime settings are saved for the next restart." color="text-dim" wrap />
      {field('Default shell', configuration.shell ?? '', 'Automatic when empty', (event) => change('shell', nullable(event.value)))}
      {field('CPU limit', numbers.cpus, 'Unlimited when empty', (event) => numeric('cpus', event.value))}
      {field('Memory (MB)', numbers.memory, 'Unlimited when empty', (event) => numeric('memory', event.value))}
      {field('Scrollback lines', numbers.scrollback, 'Unlimited when empty', (event) => numeric('scrollback', event.value))}
      <Column gap={1}><Text label="Execution lifetime" /><Select value={configuration.execution_lifetime} choices={[
        { value: 'persisted', label: 'Persisted across restarts' }, { value: 'live', label: 'Live until shutdown' },
        { value: 'ephemeral', label: 'Ephemeral per execution' },
      ]} onChange={(event: Change) => change('execution_lifetime', String(event.value ?? 'persisted') as WorkspaceConfiguration['execution_lifetime'])} /></Column>
      {field('VPN proxy', configuration.vpn ?? '', 'socks5://host:port (optional)', (event) => change('vpn', nullable(event.value)))}
      <Row gap={2} align="center"><Switch checked={configuration.docker_socket} onToggle={(event: Change) => change('docker_socket', Boolean(event.value))} /><Text label="Expose Docker-compatible workspace socket" /></Row>
    </SettingsGroup>
    <SettingsGroup name="terminal" label="Terminal appearance" detail={terminalSummary(configuration)} expanded={expanded} onExpand={setExpanded}>
      {field('Font family', configuration.terminal.font_family ?? '', 'Host default', (event) => terminal('font_family', nullable(event.value)))}
      {field('Font size', numbers.fontSize, 'Host default', (event) => numeric('fontSize', event.value))}
      {colorField('Foreground', configuration.terminal.foreground, (value) => terminal('foreground', value))}
      {colorField('Background', configuration.terminal.background, (value) => terminal('background', value))}
      <Column gap={1}><Text label="Cursor shape" /><Select value={configuration.terminal.cursor_shape ?? ''} choices={[
        { value: '', label: 'Host default' }, { value: 'block', label: 'Block' }, { value: 'ibeam', label: 'I-beam' }, { value: 'underline', label: 'Underline' },
      ]} onChange={(event: Change) => terminal('cursor_shape', nullable(event.value))} /></Column>
      <Row gap={2} align="center">
        <Switch checked={configuration.terminal.cursor_blink ?? false} onToggle={(event: Change) => terminal('cursor_blink', Boolean(event.value))} />
        <Text label="Cursor blink" />
        <Button label="Use host default for cursor blink" enabled={configuration.terminal.cursor_blink !== null}
          onInvoke={() => terminal('cursor_blink', null)} />
      </Row>
      {configuration.terminal.cursor_blink === null ? <Text label="Cursor blink uses the host default." color="text-dim" /> : null}
    </SettingsGroup>
    <SettingsGroup name="environment" label="Environment variables" detail={`${configuration.environment.length} configured`} expanded={expanded} onExpand={setExpanded}>
      <Environment values={configuration.environment} onChange={(value) => change('environment', value)} />
    </SettingsGroup>
    <SettingsGroup name="mounts" label="Filesystem mounts" detail={`${configuration.mounts.length} configured`} expanded={expanded} onExpand={setExpanded}>
      <Mounts values={configuration.mounts} onChange={(value) => change('mounts', value)} />
    </SettingsGroup>
    {invalid && <InlineMessage label={invalid} tone="danger" />}{error && <InlineMessage label={error} tone="danger" />}{saved && <InlineMessage label={saved} tone="positive" />}
    <CardActions><Button label={saving ? 'Saving…' : 'Save workspace'} enabled={!saving && dirty && !invalid} onInvoke={save} /><Button label="Discard changes" enabled={!saving && dirty} onInvoke={load} /></CardActions>
  </Column></Scroll>;
}

function SettingsGroup({ name, label, detail, expanded, onExpand, children }: { name: string; label: string; detail: string; expanded: string; onExpand: (value: string) => void; children: React.ReactNode }) {
  const open = expanded === name;
  return <Accordion label={label} expanded={open} onExpand={(event: Change) => onExpand(Boolean(event.expanded ?? event.value) ? name : '')}>
    <AccordionSummary label={label}><Text label={detail} color="text-dim" wrap /></AccordionSummary>
    <AccordionDetails gap={2}>{children}</AccordionDetails>
  </Accordion>;
}

function Environment({ values, onChange }: { values: [string, string][]; onChange: (value: [string, string][]) => void }) {
  const replace = (index: number, part: 0 | 1, value: unknown) => onChange(values.map((row, at) => at === index ? [part === 0 ? String(value ?? '') : row[0], part === 1 ? String(value ?? '') : row[1]] : row));
  return <Column gap={2}>{values.map((row, index) => <Row key={`${index}:${row[0]}`} gap={1}><Entry value={row[0]} placeholder="NAME" onChange={(event: Change) => replace(index, 0, event.value)} /><Entry value={row[1]} placeholder="value" grow onChange={(event: Change) => replace(index, 1, event.value)} /><Button label={`Remove ${row[0] || `variable ${index + 1}`}`} onInvoke={() => onChange(values.filter((_, at) => at !== index))} /></Row>)}<CardActions><Button label="Add variable" onInvoke={() => onChange([...values, ['', '']])} /></CardActions></Column>;
}
function Mounts({ values, onChange }: { values: WorkspaceMount[]; onChange: (value: WorkspaceMount[]) => void }) {
  const replace = (index: number, patch: Partial<WorkspaceMount>) => onChange(values.map((row, at) => at === index ? { ...row, ...patch } : row));
  return <Column gap={2}>{values.map((mount, index) => <Column key={`${index}:${mount.container}`} gap={1}><Row gap={1}><Entry value={mount.host} placeholder="Host path" grow onChange={(event: Change) => replace(index, { host: String(event.value ?? '') })} /><Entry value={mount.container} placeholder="Absolute container path" grow onChange={(event: Change) => replace(index, { container: String(event.value ?? '') })} /></Row><Row gap={2} align="center"><Switch checked={mount.read_only} onToggle={(event: Change) => replace(index, { read_only: Boolean(event.value) })} /><Text label="Read only" /><Button label={`Remove mount ${index + 1}`} onInvoke={() => onChange(values.filter((_, at) => at !== index))} /></Row></Column>)}<CardActions><Button label="Add mount" onInvoke={() => onChange([...values, { host: '', container: '', read_only: true }])} /></CardActions></Column>;
}
function field(label: string, value: string, placeholder: string, onChange: (event: Change) => void) { return <Column gap={1}><Text label={label} /><Entry value={value} placeholder={placeholder} onChange={onChange} /></Column>; }
function colorField(label: string, value: string | null, onChange: (value: string | null) => void) { return <Column gap={1}><Text label={label} /><Row gap={1} align="center"><ColorPicker value={value ?? '#000000'} onChange={(event: Change) => onChange(nullable(event.value))} /><Button label={`Use host default for ${label.toLowerCase()}`} enabled={value !== null} onInvoke={() => onChange(null)} /></Row>{value === null && <Text label="Host default" color="text-dim" />}</Column>; }
function nullable(value: unknown): string | null { const result = String(value ?? '').trim(); return result || null; }
function numberDraft(value: WorkspaceConfiguration): Numbers { return { cpus: text(value.cpus), memory: text(value.memory_mb), scrollback: text(value.scrollback), fontSize: text(value.terminal.font_size) }; }
function text(value: number | null): string { return value === null ? '' : String(value); }
function terminalSummary(value: WorkspaceConfiguration): string { return `${value.terminal.font_family ?? 'Host font'} · ${value.terminal.font_size ?? 'default size'} · ${value.terminal.cursor_shape ?? 'default cursor'}`; }
function optionalInteger(value: string, label: string): number | null { if (!value.trim()) return null; const parsed = Number(value); if (!Number.isSafeInteger(parsed) || parsed <= 0) throw new Error(`${label} must be a positive whole number or empty.`); return parsed; }
function withNumbers(value: WorkspaceConfiguration, numbers: Numbers): WorkspaceConfiguration { return { ...value, cpus: optionalInteger(numbers.cpus, 'CPU limit'), memory_mb: optionalInteger(numbers.memory, 'Memory'), scrollback: optionalInteger(numbers.scrollback, 'Scrollback'), terminal: { ...value.terminal, font_size: optionalInteger(numbers.fontSize, 'Font size') } }; }
function validate(value: WorkspaceConfiguration) {
  if (!value.image.trim()) throw new Error('Workspace image must not be empty.');
  for (const [label, color] of [['Foreground', value.terminal.foreground], ['Background', value.terminal.background]] as const) if (color && !/^#[0-9a-fA-F]{6}$/.test(color)) throw new Error(`${label} must use #RRGGBB.`);
  const names = new Set<string>(); for (const [name] of value.environment) { if (!/^[A-Za-z_][A-Za-z0-9_]*$/.test(name)) throw new Error('Environment names must use letters, digits and underscores and cannot start with a digit.'); if (names.has(name)) throw new Error(`Environment variable ${name} is duplicated.`); names.add(name); }
  const targets = new Set<string>(); for (const mount of value.mounts) { if (!mount.host.trim() || !mount.container.startsWith('/') || mount.container.includes('/../')) throw new Error('Every mount needs a host path and a normalized absolute container path.'); if (targets.has(mount.container)) throw new Error(`Mount target ${mount.container} is duplicated.`); targets.add(mount.container); }
}
function validationMessage(configuration: WorkspaceConfiguration, numbers: Numbers): string { try { validate(withNumbers(configuration, numbers)); return ''; } catch (cause) { return message(cause); } }
function message(cause: unknown): string { return cause instanceof Error ? cause.message.slice(0, 500) : String(cause).slice(0, 500); }
