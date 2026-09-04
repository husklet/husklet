import React from 'react';
import {
  Button, Card, CardActions, CardContent, CardHeader, Column, Entry, Heading, Meter,
  ObjectInspector, ResourceState, Row, Scroll, Spinner, Text,
  type ImageDetails, type ImagePullStatus, type ImageSummary, type WorkspaceApi,
} from '@husklet/react';
import { ImageDetailsSource, bounded, boundedMessage, bytes, shortId } from './model.js';
import type { Resource } from './overview.js';

const INSPECTOR_BOUNDS = Object.freeze({ maxDepth: 8, maxNodes: 128, maxStringLength: 256 });
const TERMINAL_PULL_STATES = new Set(['complete', 'failed', 'cancelled']);
type Inspection = {
  id: string;
  state: 'idle' | 'loading' | 'ready' | 'error';
  count: number;
  error: unknown;
};

export function Images({ api, resource, imageDetails }: {
  api: WorkspaceApi;
  resource: Resource<ImageSummary>;
  imageDetails?: ImageDetailsSource;
}) {
  const localDetails = React.useMemo(() => new ImageDetailsSource(), []);
  const detailsSource = imageDetails ?? localDetails;
  const [reference, setReference] = React.useState('');
  const [detail, setDetail] = React.useState<ImageDetails | null>(null);
  const [confirm, setConfirm] = React.useState('');
  const [busy, setBusy] = React.useState('');
  const [error, setError] = React.useState<unknown>(null);
  const [notice, setNotice] = React.useState('');
  const inspectionRevision = React.useRef(0);
  const inventoryRevision = React.useRef(resource.data);
  const currentImages = React.useRef(new Set<string>());
  currentImages.current = new Set((resource.data ?? []).map((item) => item.id));
  const [pull, setPull] = React.useState<ImagePullStatus | null>(null);
  const pullRevision = React.useRef(0);
  const completedPull = React.useRef('');
  const activePullJob = pull?.job && !TERMINAL_PULL_STATES.has(pull.state) ? pull.job : '';
  const [inspection, setInspection] = React.useState<Inspection>({ id: '', state: 'idle', count: 0, error: null });

  const run = async (name: string, operation: () => void | Promise<void>) => {
    setBusy(name);
    setError(null);
    setNotice('');
    try { await operation(); } catch (cause) { setError(cause); } finally { setBusy(''); }
  };
  const startPull = () => run('pull', async () => {
    const requested = reference.trim();
    const started = await api.images.startPull(requested);
    pullRevision.current = 0;
    completedPull.current = '';
    setPull({
      job: started.job, reference: requested, revision: 0, state: 'starting', status: 'Starting pull…',
      layer: null, current: null, total: null, image: null, error: null,
    });
  });

  React.useEffect(() => {
    if (!activePullJob) return undefined;
    let disposed = false;
    let stop: (() => void | Promise<void>) | null = null;
    const accept = async (status: ImagePullStatus, minimumRevision = 0) => {
      if (disposed || status.job !== activePullJob
        || status.revision < minimumRevision || status.revision < pullRevision.current) return;
      pullRevision.current = status.revision;
      setPull(status);
      if (status.state === 'complete' && completedPull.current !== status.job) {
        completedPull.current = status.job;
        setNotice(`Pulled ${status.reference}.`);
        await resource.reload();
      }
    };
    void api.watchImagePulls(async (change) => {
      if (disposed || change.job !== activePullJob || change.revision <= pullRevision.current) return;
      const status = await api.images.pullStatus(activePullJob);
      await accept(status, change.revision);
    }).then(async (dispose) => {
      if (disposed) { void dispose(); return; }
      stop = dispose;
      // Subscription acknowledgement is the ordering boundary. Reading after
      // it closes both gaps: a cache-hit completion before subscription, and a
      // change between the first render and the subscription acknowledgement.
      await accept(await api.images.pullStatus(activePullJob));
    }).catch((cause: unknown) => {
      if (!disposed) setPull((current) => current
        ? { ...current, state: 'failed', error: boundedMessage(cause) }
        : current);
    });
    return () => { disposed = true; if (stop) void stop(); };
  }, [activePullJob, api, resource.reload]);

  const cancelPull = () => run('pull-cancel', async () => {
    if (!pull) return;
    await api.images.cancelPull(pull.job);
    setPull((current) => current ? { ...current, state: 'cancelled', status: 'Pull cancelled.' } : current);
  });
  const inspect = async (item: ImageSummary) => {
    const revision = ++inspectionRevision.current;
    setBusy(`inspect:${item.id}`);
    setDetail(null);
    setInspection({ id: item.id, state: 'loading', count: 0, error: null });
    try {
      const value = await api.images.inspect(item.reference || item.id);
      if (revision !== inspectionRevision.current) return;
      const count = await detailsSource.replace(value);
      if (revision !== inspectionRevision.current) return;
      setDetail(value);
      setInspection({ id: item.id, state: 'ready', count, error: null });
    } catch (cause) {
      if (revision === inspectionRevision.current) setInspection({ id: item.id, state: 'error', count: 0, error: cause });
    } finally {
      if (revision === inspectionRevision.current) setBusy('');
    }
  };
  React.useEffect(() => {
    if (inventoryRevision.current === resource.data) return;
    inventoryRevision.current = resource.data;
    inspectionRevision.current += 1;
    setDetail(null);
    setInspection({ id: '', state: 'idle', count: 0, error: null });
    setConfirm('');
  }, [resource.data]);

  const remove = (item: ImageSummary) => run(`remove:${item.id}`, async () => {
    if (!currentImages.current.has(item.id)) throw new Error(`Image ${item.id} changed or disappeared; inspect and confirm again.`);
    await api.images.remove(item.id);
    setConfirm('');
    if (detail?.id === item.id) setDetail(null);
    await resource.reload();
  });
  const prune = () => run('prune', async () => {
    const result = await api.images.prune();
    setConfirm('');
    setNotice(`Pruned ${result.deleted} image records and reclaimed ${bytes(result.space_reclaimed)}.`);
    await resource.reload();
  });

  const view = bounded(resource.data);
  const inventoryState: 'loading' | 'error' | 'empty' | 'ready' = resource.loading
    ? 'loading' : resource.error ? 'error' : view.records.length === 0 ? 'empty' : 'ready';
  return <Page title="Images" subtitle="Images available to this workspace.">
    <Row gap={1}>
      <Entry value={reference} placeholder="registry/image:tag"
        onChange={(event) => setReference(String(event.value ?? ''))} />
      <Button label={pull?.state === 'failed' ? 'Retry pull' : busy === 'pull' ? 'Starting…' : 'Pull'}
        enabled={!busy && reference.trim().length > 0 && (!pull || TERMINAL_PULL_STATES.has(pull.state))}
        onInvoke={startPull} />
      <Button label="Refresh" enabled={!busy} onInvoke={resource.reload} />
    </Row>
    {pull ? <PullStatus pull={pull} onCancel={cancelPull} /> : null}
    <ErrorText error={error} />
    {notice ? <Text label={notice} color="positive" /> : null}
    <ResourceState state={inventoryState} loadingLabel="Reading images…" emptyLabel="No images"
      emptyDetail="Enter an image reference above to pull one into this workspace."
      error={boundedMessage(resource.error)} retryLabel="Retry images" onRetry={resource.reload}>
      <Row gap={1} align="center">
        {busy ? <Spinner /> : null}
        {confirm === 'prune' ? <>
          <Text label="Remove every unused image?" color="warning" />
          <Button label="Confirm prune" enabled={!busy} tone="danger" destructive onInvoke={prune} />
          <Button label="Cancel" enabled={!busy} onInvoke={() => setConfirm('')} />
        </> : <Button label="Prune unused images" enabled={!busy} tone="danger" onInvoke={() => setConfirm('prune')} />}
      </Row>
      {view.records.map((item) => <Card key={item.id} variant={detail?.id === item.id ? 'filled' : 'outline'}>
        <CardHeader label={item.reference || '<untagged>'} detail={shortId(item.id)} />
        <CardContent>
          <Text label={bytes(item.size)} color="text-dim" />
          {inspection.id === item.id ? <ResourceState
            state={inspection.state === 'idle' ? 'loading'
              : inspection.state === 'ready' && inspection.count === 0 ? 'empty' : inspection.state}
            loadingLabel="Reading image details…" emptyLabel="No image details"
            emptyDetail="The host returned no inspectable fields." error={boundedMessage(inspection.error)}
            retryLabel="Retry inspect" onRetry={() => inspect(item)}>
            <StructuredDetail value={detail} />
          </ResourceState> : null}
        </CardContent>
        <CardActions gap={1}>
          <Button label="Inspect" enabled={!busy} onInvoke={() => inspect(item)} />
          {confirm === item.id ? <>
            <Text label={`Remove immutable image ${item.id}?`} color="warning" />
            <Button label="Confirm remove" enabled={!busy} tone="danger" destructive onInvoke={() => remove(item)} />
            <Button label="Cancel" enabled={!busy} onInvoke={() => setConfirm('')} />
          </> : <Button label="Remove" enabled={!busy} tone="danger" onInvoke={() => setConfirm(item.id)} />}
        </CardActions>
      </Card>)}
      <Omitted count={view.omitted} />
    </ResourceState>
  </Page>;
}

function PullStatus({ pull, onCancel }: { pull: ImagePullStatus; onCancel: () => void | Promise<void> }) {
  const determinate = pull.total !== null && pull.current !== null && pull.total > 0;
  return <Card variant={pull.state === 'failed' ? 'outline' : 'filled'}>
    <CardHeader label={pull.reference} detail={pull.status ?? pull.state} />
    <CardContent gap={1}>
      {determinate ? <Meter fraction={Math.min(1, pull.current! / pull.total!)}
        value={`${pull.current} / ${pull.total} bytes`} />
        : pull.state === 'pulling' || pull.state === 'starting' ? <Spinner /> : null}
      {pull.layer ? <Text label={`Layer ${pull.layer}`} color="text-dim" /> : null}
      {pull.error ? <Text label={pull.error} color="danger" wrap /> : null}
    </CardContent>
    <CardActions>{!TERMINAL_PULL_STATES.has(pull.state) ? <Button label="Cancel pull" onInvoke={onCancel} /> : null}</CardActions>
  </Card>;
}

function StructuredDetail({ value }: { value: unknown }) {
  return <ObjectInspector value={value} {...INSPECTOR_BOUNDS}
    height={{ minimum: { step: 10 }, maximum: { step: 32 } }} />;
}
function Page({ title, subtitle, children }: { title: string; subtitle: string; children: React.ReactNode }) {
  return <Scroll grow height="fill"><Column pad={4} gap={2}>
    <Heading label={title} scale="title" /><Text label={subtitle} color="text-dim" wrap />{children}
  </Column></Scroll>;
}
function ErrorText({ error }: { error: unknown }) {
  return error ? <Text label={boundedMessage(error)} color="danger" wrap /> : null;
}
function Omitted({ count }: { count: number }) { return count > 0 ? <Text label={`${count} more records omitted to keep this view bounded.`} color="text-dim" /> : null; }
