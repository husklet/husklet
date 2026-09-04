import React from 'react';
import {
  Button, Card, CardActions, CardContent, CardHeader, Column, ConfirmAction, EmptyState, Entry,
  Heading, ObjectInspector, ResourceState, Row, Scroll, Spinner, Text,
  type VolumeSummary, type WorkspaceApi,
} from '@husklet/react';
import { VolumeDetailsSource, bounded, boundedMessage } from './model.js';
import type { Resource } from './overview.js';

const INSPECTOR_BOUNDS = Object.freeze({ maxDepth: 8, maxNodes: 128, maxStringLength: 256 });
type Inspection = {
  name: string;
  state: 'idle' | 'loading' | 'ready' | 'error';
  count: number;
  detail: VolumeSummary | null;
  error: unknown;
};
type Creation = {
  state: 'idle' | 'loading' | 'success' | 'error';
  name: string;
  error: unknown;
};
const EMPTY_INSPECTION: Inspection = { name: '', state: 'idle', count: 0, detail: null, error: null };

export function Volumes({ api, resource, volumeDetails }: {
  api: WorkspaceApi;
  resource: Resource<VolumeSummary>;
  volumeDetails?: VolumeDetailsSource;
}) {
  const localDetails = React.useMemo(() => new VolumeDetailsSource(), []);
  const detailsSource = volumeDetails ?? localDetails;
  const [name, setName] = React.useState('');
  const [inspection, setInspection] = React.useState<Inspection>(EMPTY_INSPECTION);
  const [creation, setCreation] = React.useState<Creation>({ state: 'idle', name: '', error: null });
  const inspectionRevision = React.useRef(0);
  const inventoryRevision = React.useRef(resource.data);
  const currentVolumes = React.useRef(new Map<string, string>());
  currentVolumes.current = new Map((resource.data ?? []).map((volume) => [volume.name, volume.generation]));

  const create = async () => {
    const requested = name.trim();
    if (!requested || creation.state === 'loading') return;
    setCreation({ state: 'loading', name: requested, error: null });
    try {
      await api.volumes.create(requested);
      await resource.reload();
      setName('');
      setCreation({ state: 'success', name: requested, error: null });
    } catch (cause) {
      setCreation({ state: 'error', name: requested, error: cause });
    }
  };
  const remove = async (volume: VolumeSummary) => {
    if (currentVolumes.current.get(volume.name) !== volume.generation) {
      throw new Error(`Volume ${volume.name} changed generation; inspect and confirm again.`);
    }
    await api.volumes.remove(volume.name, volume.generation);
    if (inspection.name === volume.name) setInspection(EMPTY_INSPECTION);
    await resource.reload();
  };
  const inspect = async (volume: VolumeSummary) => {
    const revision = ++inspectionRevision.current;
    setInspection({ name: volume.name, state: 'loading', count: 0, detail: null, error: null });
    try {
      const detail = await api.volumes.inspect(volume.name);
      if (revision !== inspectionRevision.current) return;
      const count = await detailsSource.replace(detail);
      if (revision !== inspectionRevision.current) return;
      setInspection({ name: volume.name, state: 'ready', count, detail, error: null });
    } catch (error) {
      if (revision === inspectionRevision.current) {
        setInspection({ name: volume.name, state: 'error', count: 0, detail: null, error });
      }
    }
  };
  React.useEffect(() => {
    if (inventoryRevision.current === resource.data) return;
    inventoryRevision.current = resource.data;
    inspectionRevision.current += 1;
    setInspection(EMPTY_INSPECTION);
  }, [resource.data]);

  const view = bounded(resource.data);
  const inventoryState: 'loading' | 'error' | 'empty' | 'ready' = resource.loading
    ? 'loading' : resource.error ? 'error' : view.records.length === 0 ? 'empty' : 'ready';
  return <Page title="Volumes" subtitle="Bounded local volume inventory and safe, non-force lifecycle.">
    <Row gap={1}>
      <Entry value={name} placeholder="Volume name" enabled={creation.state !== 'loading'}
        onChange={(event) => {
          setName(String(event.value ?? ''));
          setCreation({ state: 'idle', name: '', error: null });
        }} />
      <Button label={creation.state === 'loading' ? 'Creating…' : creation.state === 'error' ? 'Retry create' : 'Create'}
        enabled={creation.state !== 'loading' && name.trim().length > 0} onInvoke={() => void create()} />
      <Button label="Refresh" enabled={creation.state !== 'loading'} onInvoke={resource.reload} />
    </Row>
    {creation.state === 'loading' ? <Row gap={1} align="center">
      <Spinner /><Text label={`Creating volume ${creation.name}…`} />
    </Row> : null}
    {creation.state === 'error' ? <Text label={boundedMessage(creation.error)} color="danger" wrap /> : null}
    {creation.state === 'success' ? <Text label={`Created volume ${creation.name}.`} color="positive" wrap /> : null}
    <ResourceState state={inventoryState} loadingLabel="Reading volumes…" emptyLabel="No volumes"
      emptyDetail="Create a named volume above when a workload needs durable storage."
      error={boundedMessage(resource.error)} retryLabel="Retry volumes" onRetry={resource.reload}>
      {view.records.map((volume) => <Card key={`${volume.name}:${volume.generation}`}
        variant={inspection.name === volume.name ? 'filled' : 'outline'}>
        <CardHeader label={volume.name} detail={volume.driver} />
        <CardActions gap={1}>
          <Button label={inspection.name === volume.name && inspection.state === 'error' ? 'Retry inspect' : 'Inspect'}
            onInvoke={() => inspect(volume)} />
          <ConfirmAction authorityKey={`volume:${volume.name}:${volume.generation}:remove`} label="Remove"
            confirmLabel="Confirm remove" pendingLabel="Confirm remove"
            question={`Remove volume ${volume.name} generation ${volume.generation}?`}
            onConfirm={() => remove(volume)} />
        </CardActions>
        {inspection.name === volume.name ? <VolumeDetail inspection={inspection} /> : null}
      </Card>)}
      <Omitted count={view.omitted} />
    </ResourceState>
  </Page>;
}

function VolumeDetail({ inspection }: { inspection: Inspection }) {
  return <CardContent>
    {inspection.state === 'loading' ? <Row gap={1} align="center">
      <Spinner /><Text label="Reading volume details…" />
    </Row> : inspection.state === 'error'
      ? <Text label={boundedMessage(inspection.error)} color="danger" wrap />
      : inspection.count === 0
        ? <EmptyState label="No volume details" detail="The host returned no inspectable fields." />
        : <ObjectInspector value={inspection.detail} {...INSPECTOR_BOUNDS}
          height={{ minimum: { step: 10 }, maximum: { step: 32 } }} />}
  </CardContent>;
}

function Page({ title, subtitle, children }: { title: string; subtitle: string; children: React.ReactNode }) {
  return <Scroll grow height="fill"><Column pad={4} gap={2}>
    <Heading label={title} scale="title" /><Text label={subtitle} color="text-dim" wrap />{children}
  </Column></Scroll>;
}
function Omitted({ count }: { count: number }) { return count > 0 ? <Text label={`${count} more records omitted to keep this view bounded.`} color="text-dim" /> : null; }
