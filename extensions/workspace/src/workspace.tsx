import React from 'react';
import {
  Button, Card, CardActions, CardContent, CardHeader, Column, Entry, Heading,
  InlineMessage, Row, Scroll, Spinner, Switch, Text,
  type WorkspaceApi, type WorkspaceConfiguration,
} from '@husklet/react';

type Change = { value?: unknown };

export function Workspace({ api }: { api: WorkspaceApi }) {
  const [configuration, setConfiguration] = React.useState<WorkspaceConfiguration | null>(null);
  const [error, setError] = React.useState('');
  const [saving, setSaving] = React.useState(false);
  const [saved, setSaved] = React.useState('');

  const load = React.useCallback(async () => {
    try {
      const current = await api.info();
      setConfiguration(await api.inspect(current.name));
      setError('');
    } catch (cause) {
      setError(message(cause));
    }
  }, [api]);
  React.useEffect(() => { void load(); }, [load]);

  const change = <K extends keyof WorkspaceConfiguration>(key: K, value: WorkspaceConfiguration[K]) => {
    setConfiguration((current) => current && ({ ...current, [key]: value }));
    setSaved('');
  };
  const terminal = <K extends keyof WorkspaceConfiguration['terminal']>(key: K, value: WorkspaceConfiguration['terminal'][K]) => {
    setConfiguration((current) => current && ({ ...current, terminal: { ...current.terminal, [key]: value } }));
    setSaved('');
  };
  const save = async () => {
    if (!configuration || saving) return;
    if (!configuration.generation) {
      setError('The host did not provide a workspace generation; reload before saving.');
      return;
    }
    setSaving(true);
    try {
      const updated = await api.update(configuration.name, configuration.generation, configuration);
      setConfiguration(updated);
      setSaved('Workspace settings saved.');
      setError('');
    } catch (cause) {
      setError(message(cause));
    } finally {
      setSaving(false);
    }
  };

  if (!configuration) return (
    <Column pad={4} gap={2}>
      <Heading label="Workspace" scale="title" />
      {error
        ? <InlineMessage label={error} tone="danger" />
        : <Row gap={2}><Spinner /><Text label="Loading workspace settings…" /></Row>}
      <Button label="Retry" enabled={Boolean(error)} onInvoke={load} />
    </Column>
  );

  return (
    <Scroll grow height="fill">
      <Column pad={4} gap={3}>
        <Heading label="Workspace" scale="title" />
        <Text label="The default overview and configuration for this workspace." color="text-dim" wrap />
        <Card variant="outline">
          <CardHeader label="Environment" detail={`${configuration.image} · linux/${configuration.architecture}`} />
          <CardContent gap={2}>
            {field('Default shell', configuration.shell ?? '', 'Automatic when empty', (event) => change('shell', nullable(event.value)))}
            {field('CPU limit', configuration.cpus?.toString() ?? '', 'Unlimited when empty', (event) => change('cpus', positiveInteger(event.value)))}
            {field('Memory (MB)', configuration.memory_mb?.toString() ?? '', 'Unlimited when empty', (event) => change('memory_mb', positiveInteger(event.value)))}
            {field('Scrollback lines', configuration.scrollback?.toString() ?? '', 'Unlimited when empty', (event) => change('scrollback', positiveInteger(event.value)))}
          </CardContent>
        </Card>
        <Card variant="outline">
          <CardHeader label="Terminal appearance" detail="Applied to new and existing workspace panes." />
          <CardContent gap={2}>
            {field('Font family', configuration.terminal.font_family ?? '', 'Host default', (event) => terminal('font_family', nullable(event.value)))}
            {field('Font size', configuration.terminal.font_size?.toString() ?? '', 'Host default', (event) => terminal('font_size', positiveInteger(event.value)))}
            {field('Foreground', configuration.terminal.foreground ?? '', '#RRGGBB or host default', (event) => terminal('foreground', nullable(event.value)))}
            {field('Background', configuration.terminal.background ?? '', '#RRGGBB or host default', (event) => terminal('background', nullable(event.value)))}
            <Row gap={2}><Text label="Cursor blink" /><Switch checked={configuration.terminal.cursor_blink ?? false} onToggle={(event: Change) => terminal('cursor_blink', Boolean(event.value))} /></Row>
          </CardContent>
        </Card>
        {error && <InlineMessage label={error} tone="danger" />}
        {saved && <InlineMessage label={saved} tone="positive" />}
        <CardActions><Button label={saving ? 'Saving…' : 'Save workspace'} enabled={!saving} onInvoke={save} /></CardActions>
      </Column>
    </Scroll>
  );
}

function field(label: string, value: string, placeholder: string, onChange: (event: Change) => void) {
  return <Column gap={1}><Text label={label} /><Entry value={value} placeholder={placeholder} onChange={onChange} /></Column>;
}

function nullable(value: unknown): string | null {
  const text = String(value ?? '').trim();
  return text || null;
}

function positiveInteger(value: unknown): number | null {
  const text = String(value ?? '').trim();
  if (!text) return null;
  const parsed = Number(text);
  return Number.isSafeInteger(parsed) && parsed > 0 ? parsed : null;
}

function message(cause: unknown): string {
  return cause instanceof Error ? cause.message.slice(0, 500) : String(cause).slice(0, 500);
}
