import React from 'react';
import {
  Badge, Button, Card, CardActions, CardContent, CardHeader, Column, ConfirmAction,
  EmptyState, Entry, Heading, InlineMessage, Row, Scroll, Spinner, Text,
  type ExtensionAcquisitionStatus, type ExtensionSummary, type WorkspaceApi,
} from '@husklet/react';

type Change = { value?: unknown };

export function Extensions({ api }: { api: WorkspaceApi }) {
  const [installed, setInstalled] = React.useState<ExtensionSummary[]>([]);
  const [reference, setReference] = React.useState('');
  const [acquisition, setAcquisition] = React.useState<ExtensionAcquisitionStatus | null>(null);
  const [busy, setBusy] = React.useState('');
  const [error, setError] = React.useState('');

  const reload = React.useCallback(async () => {
    try { setInstalled(await api.extensions.list()); setError(''); }
    catch (cause) { setError(message(cause)); }
  }, [api]);
  React.useEffect(() => { void reload(); }, [reload]);
  React.useEffect(() => {
    let dispose: (() => Promise<void>) | undefined;
    void api.watchExtensions((listing) => setInstalled(listing)).then((stop) => { dispose = stop; }).catch(() => {});
    return () => { void dispose?.(); };
  }, [api]);

  const inspect = async () => {
    const wanted = reference.trim();
    if (!wanted || busy) return;
    setBusy('inspect'); setError(''); setAcquisition(null);
    try {
      const started = await api.extensions.startAcquisition(wanted);
      for (let attempt = 0; attempt < 120; attempt += 1) {
        const status = await api.extensions.acquisition(started.job);
        setAcquisition(status);
        if (status.state === 'ready' || status.state === 'failed' || status.state === 'cancelled') break;
        await new Promise((resolve) => setTimeout(resolve, 250));
      }
    } catch (cause) { setError(message(cause)); }
    finally { setBusy(''); }
  };
  const install = async () => {
    if (!acquisition?.candidate || busy) return;
    setBusy('install');
    try {
      await api.extensions.install(acquisition.job, acquisition.revision, acquisition.candidate.requested);
      setAcquisition(null); setReference(''); await reload();
    } catch (cause) { setError(message(cause)); }
    finally { setBusy(''); }
  };
  const lifecycle = async (extension: ExtensionSummary, action: 'enable' | 'disable' | 'retry' | 'remove') => {
    setBusy(`${action}:${extension.name}`);
    try { await api.extensions[action](extension.name, extension.image_digest); await reload(); }
    catch (cause) { setError(message(cause)); }
    finally { setBusy(''); }
  };

  return (
    <Scroll grow height="fill">
      <Column pad={4} gap={3}>
        <Heading label="Extensions" scale="title" />
        <Text label="Install, update, enable, disable, and remove workspace extensions." color="text-dim" wrap />
        <Card variant="outline">
          <CardHeader label="Install an extension" detail="OCI image reference" />
          <CardContent>
            <Row gap={1}>
              <Entry value={reference} placeholder="registry.example/extension:version" onChange={(event: Change) => setReference(String(event.value ?? '').slice(0, 512))} />
              <Button label={busy === 'inspect' ? 'Inspecting…' : 'Inspect'} enabled={Boolean(reference.trim()) && !busy} onInvoke={inspect} />
            </Row>
          </CardContent>
          {acquisition?.candidate && (
            <CardContent gap={1}>
              <Text label={`${acquisition.candidate.name} ${acquisition.candidate.version}`} />
              <Text label={acquisition.candidate.image_digest} wrap />
              <Text label={`Requested: ${acquisition.candidate.requested.join(', ') || 'nothing'}`} wrap />
              <Button label={busy === 'install' ? 'Installing…' : 'Install with requested capabilities'} enabled={!busy} onInvoke={install} />
            </CardContent>
          )}
          {acquisition && !acquisition.candidate && (
            <CardContent><Row gap={2}><Spinner /><Text label={acquisition.state} /></Row></CardContent>
          )}
        </Card>
        {error && <InlineMessage label={error} tone="danger" />}
        <Heading label="Installed" scale="title" />
        {installed.length === 0 && <EmptyState label="No extensions installed" detail="Install an OCI extension above." />}
        {installed.map((extension) => (
          <Card key={`${extension.name}:${extension.image_digest}`} variant="outline">
            <CardHeader label={extension.name} detail={extension.version ?? extension.image_digest} />
            <CardContent><Row gap={2}><Badge label={extension.status} /><Text label={extension.image_digest} wrap /></Row></CardContent>
            <CardActions gap={1}>
              {extension.status === 'running'
                ? <Button label="Disable" enabled={!busy} onInvoke={() => lifecycle(extension, 'disable')} />
                : <Button label={extension.status === 'fault' ? 'Retry' : 'Enable'} enabled={!busy} onInvoke={() => lifecycle(extension, extension.status === 'fault' ? 'retry' : 'enable')} />}
              <ConfirmAction
                label="Remove"
                confirmLabel={`Remove ${extension.name}`}
                question={`Remove ${extension.name} from this workspace?`}
                authorityKey={extension.image_digest}
                enabled={!busy}
                onConfirm={() => lifecycle(extension, 'remove')}
              />
            </CardActions>
          </Card>
        ))}
      </Column>
    </Scroll>
  );
}

function message(cause: unknown): string {
  return cause instanceof Error ? cause.message.slice(0, 500) : String(cause).slice(0, 500);
}
