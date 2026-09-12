import React from 'react';
import {
  Button,
  Card,
  CardContent,
  Column,
  ConfirmAction,
  EmptyState,
  Entry,
  Expander,
  FormControl,
  FormLabel,
  Heading,
  IconButton,
  ResourceState,
  RecoveryState,
  Row,
  Scroll,
  Spinner,
  Text,
  type VolumeSummary,
  type WorkspaceApi,
} from '@husklet/react';
import { VolumeDetailsSource, bounded, boundedMessage } from './model.js';
import type { Resource } from './overview.js';
import { ResourceSummary } from './resource-summary.js';
import { AuthorityRecovery } from './authority-recovery.js';

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
const EMPTY_INSPECTION: Inspection = {
  name: '',
  state: 'idle',
  count: 0,
  detail: null,
  error: null,
};

export function Volumes({
  api,
  resource,
  volumeDetails,
  onOpenExtensions,
}: {
  api: WorkspaceApi;
  resource: Resource<VolumeSummary>;
  volumeDetails?: VolumeDetailsSource;
  onOpenExtensions: () => void;
}) {
  const localDetails = React.useMemo(() => new VolumeDetailsSource(), []);
  const detailsSource = volumeDetails ?? localDetails;
  const [name, setName] = React.useState('');
  const [inspection, setInspection] = React.useState<Inspection>(EMPTY_INSPECTION);
  const [creation, setCreation] = React.useState<Creation>({
    state: 'idle',
    name: '',
    error: null,
  });
  const [removalNotice, setRemovalNotice] = React.useState('');
  const inspectionRevision = React.useRef(0);
  const inventoryRevision = React.useRef(resource.data);
  const currentVolumes = React.useRef(new Map<string, string>());
  currentVolumes.current = new Map(
    (resource.data ?? []).map((volume) => [volume.name, volume.generation]),
  );

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
    setRemovalNotice('');
    if (currentVolumes.current.get(volume.name) !== volume.generation) {
      throw new Error(`Volume ${volume.name} changed generation; inspect and confirm again.`);
    }
    const removed = await api.volumes.removeAndWait(volume.name, volume.generation);
    if (inspection.name === volume.name) setInspection(EMPTY_INSPECTION);
    await resource.reload();
    setRemovalNotice(
      removed.changed
        ? `Volume ${volume.name} generation ${volume.generation} was removed and its absence was verified.`
        : `Volume ${volume.name} removal was accepted, but absence was not observed before the timeout.`,
    );
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
  const toggleInspection = async (volume: VolumeSummary) => {
    if (inspection.name === volume.name && inspection.state === 'ready') {
      inspectionRevision.current += 1;
      setInspection(EMPTY_INSPECTION);
      await detailsSource.replace(null);
      return;
    }
    await inspect(volume);
  };
  React.useEffect(() => {
    if (inventoryRevision.current === resource.data) return;
    inventoryRevision.current = resource.data;
    inspectionRevision.current += 1;
    setInspection(EMPTY_INSPECTION);
  }, [resource.data]);

  const view = bounded(resource.data);
  const inventoryState: 'loading' | 'error' | 'empty' | 'ready' = resource.loading
    ? 'loading'
    : resource.error
      ? 'error'
      : view.records.length === 0
        ? 'empty'
        : 'ready';
  return (
    <Page title="Volumes" subtitle="Bounded local volume inventory and safe, non-force lifecycle.">
      <FormControl gap={1} align="start">
        <FormLabel label="Volume name" />
        <Row gap={1} align="start" justify="center" wrap>
          <Entry
            value={name}
            placeholder="Volume name"
            width={{ minimum: { chars: 10 }, maximum: { chars: 32 } }}
            enabled={creation.state !== 'loading'}
            onChange={(event) => {
              setName(String(event.value ?? ''));
              setCreation({ state: 'idle', name: '', error: null });
            }}
          />
          <Button
            variant="filled"
            tone="accent"
            label={
              creation.state === 'loading'
                ? 'Creating…'
                : creation.state === 'error'
                  ? 'Retry create'
                  : 'Create'
            }
            size="small"
            enabled={creation.state !== 'loading' && name.trim().length > 0}
            onInvoke={() => void create()}
          />
          <IconButton
            label="Refresh"
            tooltip="Refresh volumes"
            icon="view-refresh-symbolic"
            size="medium"
            variant="ghost"
            enabled={creation.state !== 'loading'}
            onInvoke={resource.reload}
          />
        </Row>
      </FormControl>
      {creation.state === 'loading' ? (
        <Row gap={1} align="center">
          <Spinner />
          <Text label={`Creating volume ${creation.name}…`} />
        </Row>
      ) : null}
      {creation.state === 'error' ? (
        <RecoveryState operation="Creating volume" error={creation.error} />
      ) : null}
      {creation.state === 'success' ? (
        <Text label={`Created volume ${creation.name}.`} color="positive" wrap />
      ) : null}
      {removalNotice ? <Text label={removalNotice} color="positive" wrap /> : null}
      <ResourceState
        state={inventoryState}
        loadingLabel="Reading volumes…"
        emptyLabel="No volumes"
        emptyDetail="Create a named volume above when a workload needs durable storage."
        error={boundedMessage(resource.error)}
        retryLabel="Retry volumes"
        onRetry={resource.reload}
      >
        {view.records.map((volume) => {
          const inspectionNeedsAccess =
            inspection.name === volume.name &&
            inspection.state === 'error' &&
            isAuthorityDenial(inspection.error);
          return (
            <Card
              key={`${volume.name}:${volume.generation}`}
              variant={inspection.name === volume.name ? 'filled' : 'outline'}
              width="fill"
            >
              <ResourceSummary
                label={volume.name}
                detail={volume.driver}
                actions={
                  inspectionNeedsAccess ? null : (
                    <Button
                      label={
                        inspection.name !== volume.name
                          ? 'Inspect'
                          : inspection.state === 'loading'
                            ? 'Reading…'
                            : inspection.state === 'error'
                              ? 'Retry inspect'
                              : 'Hide details'
                      }
                      size="small"
                      variant={inspection.name === volume.name ? 'filled' : 'outline'}
                      tone={inspection.name === volume.name ? 'accent' : 'neutral'}
                      enabled={inspection.state !== 'loading'}
                      onInvoke={() => void toggleInspection(volume)}
                    />
                  )
                }
                overflow={
                  inspectionNeedsAccess ? null : (
                    <Expander
                      label="Danger zone"
                      variant="outline"
                      width="content"
                      align="start"
                      tooltip="Remove this volume and permanently delete its stored data"
                    >
                      <Column gap={1}>
                        <Text
                          label="Removing this volume permanently deletes its stored data."
                          color="text-dim"
                          wrap
                        />
                        <Row>
                          <ConfirmAction
                            authorityKey={`volume:${volume.name}:${volume.generation}:remove`}
                            label="Remove"
                            confirmLabel="Confirm remove"
                            pendingLabel="Confirm remove"
                            question={`Remove volume ${volume.name} generation ${volume.generation}?`}
                            size="small"
                            onConfirm={() => remove(volume)}
                          />
                        </Row>
                      </Column>
                    </Expander>
                  )
                }
              />
              {inspectionNeedsAccess ? (
                <CardContent>
                  <AuthorityRecovery resource="volume" onOpenExtensions={onOpenExtensions} />
                </CardContent>
              ) : null}
              {inspection.name === volume.name && !inspectionNeedsAccess ? (
                <VolumeDetail inspection={inspection} />
              ) : null}
            </Card>
          );
        })}
        <Omitted count={view.omitted} />
      </ResourceState>
    </Page>
  );
}

function VolumeDetail({ inspection }: { inspection: Inspection }) {
  return (
    <CardContent>
      {inspection.state === 'loading' ? (
        <Row gap={1} align="center">
          <Spinner />
          <Text label="Reading volume details…" />
        </Row>
      ) : inspection.state === 'error' ? (
        <Text label={boundedMessage(inspection.error)} color="danger" wrap />
      ) : inspection.count === 0 ? (
        <EmptyState label="No volume details" detail="The host returned no inspectable fields." />
      ) : (
        <Column gap={1}>
          <Heading label="Volume details" scale="caption" />
          <Text label={`Name · ${inspection.detail?.name}`} />
          <Text label={`Driver · ${inspection.detail?.driver}`} />
          <Text label={`Immutable generation · ${inspection.detail?.generation}`} wrap />
        </Column>
      )}
    </CardContent>
  );
}

function isAuthorityDenial(error: unknown): boolean {
  if (!error || typeof error !== 'object') return false;
  const failure = error as { kind?: unknown; message?: unknown };
  return (
    failure.kind === 'denied' ||
    (typeof failure.message === 'string' && failure.message.includes('consented resource scope'))
  );
}

function Page({
  title,
  subtitle,
  children,
}: {
  title: string;
  subtitle: string;
  children: React.ReactNode;
}) {
  return (
    <Scroll grow width="fill" height="fill">
      <Column width="fill" pad={4} gap={3}>
        <Heading label={title} scale="display" />
        <Text label={subtitle} color="text-dim" wrap />
        {children}
      </Column>
    </Scroll>
  );
}
function Omitted({ count }: { count: number }) {
  return count > 0 ? (
    <Text label={`${count} more records omitted to keep this view bounded.`} color="text-dim" />
  ) : null;
}
