import React from 'react';
import {
  Badge, Button, Card, CardActions, CardContent, CardHeader, Column, ConfirmAction,
  Entry, Heading, InlineMessage, ResourceState, Row, Scroll, Spinner, Switch, Text,
  type ExtensionAcquisitionStatus, type ExtensionCapability, type ExtensionSummary, type WorkspaceApi,
} from '@husklet/react';

type Change = { value?: unknown };

export function Extensions({ api }: { api: WorkspaceApi }) {
  const [installed, setInstalled] = React.useState<ExtensionSummary[]>([]);
  const [inventoryState, setInventoryState] = React.useState<'loading' | 'empty' | 'error' | 'ready'>('loading');
  const [inventoryError, setInventoryError] = React.useState('');
  const [watchError, setWatchError] = React.useState('');
  const [reference, setReference] = React.useState('');
  const [acquisition, setAcquisition] = React.useState<ExtensionAcquisitionStatus | null>(null);
  const [granted, setGranted] = React.useState<ExtensionCapability[]>([]);
  const [busy, setBusy] = React.useState('');
  const [error, setError] = React.useState('');
  const [notice, setNotice] = React.useState<{ label: string; uncertain: boolean } | null>(null);
  const cancelling = React.useRef(false);
  const cancelledJob = React.useRef('');
  const candidateKey = React.useRef('');
  const inventoryEpoch = React.useRef(0);

  const reload = React.useCallback(async () => {
    const epoch = ++inventoryEpoch.current;
    setInventoryState('loading'); setInventoryError('');
    try {
      const listing = await api.extensions.list();
      if (inventoryEpoch.current !== epoch) return;
      setInstalled(listing); setInventoryState(listing.length === 0 ? 'empty' : 'ready');
    } catch (cause) {
      if (inventoryEpoch.current !== epoch) return;
      setInstalled([]); setInventoryError(message(cause)); setInventoryState('error');
    }
  }, [api]);
  React.useEffect(() => { void reload(); }, [reload]);
  React.useEffect(() => {
    let dispose: (() => Promise<void>) | undefined;
    void api.watchExtensions((listing) => {
      ++inventoryEpoch.current; setInstalled(listing); setInventoryState(listing.length === 0 ? 'empty' : 'ready'); setInventoryError('');
    }).then((stop) => { dispose = stop; setWatchError(''); })
      .catch((cause) => setWatchError(`Live extension updates are unavailable: ${message(cause)} Refresh to read the current inventory.`));
    return () => { void dispose?.(); };
  }, [api]);

  const inspect = async () => {
    const wanted = reference.trim();
    if (!wanted || busy) return;
    setBusy('inspect'); setError(''); setNotice(null); setAcquisition(null); candidateKey.current = '';
    try {
      const started = await api.extensions.startAcquisition(wanted);
      cancelledJob.current = '';
      let status = await api.extensions.acquisition(started.job);
      const deadline = Date.now() + 30_000;
      while (true) {
        setAcquisition(status);
        if (status.candidate) {
          const key = `${status.job}:${status.candidate.image_digest}`;
          if (candidateKey.current !== key) { candidateKey.current = key; setGranted(status.candidate.requested); }
        }
        if (status.state === 'ready' || status.state === 'failed' || status.state === 'cancelled' || cancelledJob.current === started.job) break;
        const remaining = deadline - Date.now();
        if (remaining <= 0) break;
        const changed = await api.extensions.waitForAcquisition(started.job, status.revision, { timeoutMs: Math.min(1_000, remaining) });
        if (changed.changed) status = changed.status;
      }
      if (!['ready', 'failed', 'cancelled'].includes(status.state) && cancelledJob.current !== started.job) setError('Acquisition is still running. You can cancel it or inspect the reference again later.');
    } catch (cause) { setError(message(cause)); }
    finally { setBusy(''); }
  };
  const publish = async () => {
    if (!acquisition?.candidate || acquisition.state !== 'ready' || busy) return;
    const updating = Boolean(acquisition.candidate.installed_image_digest);
    setBusy(updating ? 'update' : 'install'); setError(''); setNotice(null);
    try {
      const result = await api.extensions[updating ? 'updateAndWait' : 'installAndWait'](acquisition.job, acquisition.revision, granted);
      setAcquisition(null); setReference(''); await reload();
      setNotice(result.changed
        ? { label: `${result.extension.name} ${updating ? 'updated' : 'installed'} and verified.`, uncertain: false }
        : { label: `${updating ? 'Update' : 'Install'} was accepted, but the resulting extension was not observed. Refresh before acting again.`, uncertain: true });
    } catch (cause) { setError(message(cause)); }
    finally { setBusy(''); }
  };
  const cancel = async () => {
    if (!acquisition || ['ready', 'failed', 'cancelled'].includes(acquisition.state) || cancelling.current) return;
    cancelling.current = true;
    cancelledJob.current = acquisition.job;
    setBusy('cancel');
    try {
      await api.extensions.cancelAcquisition(acquisition.job, acquisition.revision);
      setAcquisition(await api.extensions.acquisition(acquisition.job));
    }
    catch (cause) { setError(message(cause)); }
    finally { cancelling.current = false; setBusy(''); }
  };
  const lifecycle = async (extension: ExtensionSummary, action: 'enable' | 'disable' | 'retry' | 'remove') => {
    setBusy(`${action}:${extension.name}`); setError(''); setNotice(null);
    try {
      const result = await api.extensions[`${action}AndWait`](extension.name, extension.image_digest);
      await reload();
      setNotice(result.changed
        ? { label: `${extension.name} ${lifecycleResult(action)} and verified.`, uncertain: false }
        : { label: `${capitalize(action)} was accepted, but the resulting extension state was not observed. Refresh before acting again.`, uncertain: true });
    }
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
              <Text label="Capability access" color="text-dim" />
              {acquisition.candidate.requested.map((capability) => <Row key={capability} gap={2} align="center">
                <Switch checked={granted.includes(capability)} onToggle={(event: Change) => setGranted((current) => Boolean(event.value) ? [...new Set([...current, capability])] : current.filter((item) => item !== capability))} />
                <Text label={capability} />
              </Row>)}
              {acquisition.candidate.requested.length === 0 && <Text label="This extension requests no capabilities." />}
              <Button label={busy === 'update' ? 'Updating…' : busy === 'install' ? 'Installing…' : acquisition.candidate.installed_image_digest ? 'Update extension' : 'Install extension'} enabled={!busy && acquisition.state === 'ready'} onInvoke={publish} />
            </CardContent>
          )}
          {acquisition && acquisition.state !== 'ready' && (
            <CardContent gap={1}>
              <Row gap={2}>
                {!['failed', 'cancelled'].includes(acquisition.state) && <Spinner />}
                <Text label={acquisitionLabel(acquisition)} wrap />
                {!['failed', 'cancelled'].includes(acquisition.state)
                  ? <Button label={busy === 'cancel' ? 'Cancelling…' : 'Cancel'} enabled={busy !== 'cancel'} onInvoke={cancel} />
                  : <Button label="Dismiss" enabled={!busy} onInvoke={() => setAcquisition(null)} />}
              </Row>
              {acquisition.error && <InlineMessage label={acquisition.error} tone="danger" />}
            </CardContent>
          )}
        </Card>
        {error && <InlineMessage label={error} tone="danger" />}
        {notice && <InlineMessage label={notice.label} tone={notice.uncertain ? 'warning' : 'positive'} />}
        <Row gap={2} align="center"><Heading label="Installed" scale="title" /><Button label="Refresh" enabled={!busy && inventoryState !== 'loading'} onInvoke={reload} /></Row>
        {watchError && <InlineMessage label={watchError} tone="warning" />}
        <ResourceState state={inventoryState} loadingLabel="Loading installed extensions…" emptyLabel="No extensions installed"
          emptyDetail="Install an OCI extension above." error={inventoryError || 'Installed extensions could not be loaded.'} onRetry={reload}>
        {installed.map((extension) => (
          <Card key={`${extension.name}:${extension.image_digest}`} variant="outline">
            <CardHeader label={extension.name} detail={extension.version ?? extension.image_digest} />
            <CardContent><Row gap={2}><Badge label={extension.status} /><Text label={extension.image_digest} wrap /></Row></CardContent>
            <CardActions gap={1}>
              {extension.status.startsWith('fault:')
                ? <Button label="Retry" enabled={!busy} onInvoke={() => lifecycle(extension, 'retry')} />
                : extension.enabled
                  ? <Button label="Disable" enabled={!busy} onInvoke={() => lifecycle(extension, 'disable')} />
                  : <Button label="Enable" enabled={!busy} onInvoke={() => lifecycle(extension, 'enable')} />}
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
        </ResourceState>
      </Column>
    </Scroll>
  );
}

function message(cause: unknown): string {
  return cause instanceof Error ? cause.message.slice(0, 500) : String(cause).slice(0, 500);
}

function acquisitionLabel(acquisition: ExtensionAcquisitionStatus): string {
  const progress = acquisition.progress;
  if (!progress) return acquisition.state;
  const amount = progress.current === null ? '' : progress.total === null ? ` · ${progress.current} bytes` : ` · ${progress.current}/${progress.total} bytes`;
  return `${progress.status}${progress.id ? ` · ${progress.id}` : ''}${amount}`.slice(0, 500);
}

function lifecycleResult(action: 'enable' | 'disable' | 'retry' | 'remove'): string {
  return action === 'enable' ? 'enabled' : action === 'disable' ? 'disabled' : action === 'retry' ? 'recovered' : 'removed';
}

function capitalize(value: string): string { return `${value[0].toUpperCase()}${value.slice(1)}`; }
