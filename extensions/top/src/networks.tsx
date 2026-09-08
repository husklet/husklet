import React from 'react';
import {
  Badge,
  Button,
  Card,
  CardActions,
  CardContent,
  CardHeader,
  Column,
  ConfirmAction,
  EmptyState,
  Entry,
  Heading,
  InlineMessage,
  ResourceState,
  RecoveryState,
  Row,
  Scroll,
  Spinner,
  Text,
  type NetworkSummary,
  type WorkspaceApi,
} from '@husklet/react';
import {
  bounded,
  boundedMessage,
  endpointAliases,
  immutableContainerId,
  resourceReference,
} from './model.js';
import type { Resource } from './overview.js';

type Inspection = {
  id: string;
  state: 'idle' | 'loading' | 'ready' | 'error';
  detail: NetworkSummary | null;
  error: unknown;
};
type Creation = { state: 'idle' | 'loading' | 'success' | 'error'; name: string; error: unknown };
type EndpointRequest = {
  verb: 'connect' | 'disconnect';
  network: string;
  container: string;
  aliases: string[];
};
type Operation = {
  state: 'idle' | 'loading' | 'success' | 'error';
  request: EndpointRequest | null;
  error: unknown;
};
const EMPTY_INSPECTION: Inspection = { id: '', state: 'idle', detail: null, error: null };

export function Networks({
  api,
  resource,
}: {
  api: WorkspaceApi;
  resource: Resource<NetworkSummary>;
}) {
  const [name, setName] = React.useState('');
  const [container, setContainer] = React.useState('');
  const [aliases, setAliases] = React.useState('');
  const [inspection, setInspection] = React.useState<Inspection>(EMPTY_INSPECTION);
  const [error, setError] = React.useState<unknown>(null);
  const [creation, setCreation] = React.useState<Creation>({
    state: 'idle',
    name: '',
    error: null,
  });
  const [operation, setOperation] = React.useState<Operation>({
    state: 'idle',
    request: null,
    error: null,
  });
  const [removalNotice, setRemovalNotice] = React.useState('');
  const [disconnectRequest, setDisconnectRequest] = React.useState<EndpointRequest | null>(null);
  const inspectionRevision = React.useRef(0);
  const inventoryRevision = React.useRef(resource.data);
  const endpointInput = React.useRef({ container: '', aliases: '' });
  endpointInput.current = { container: container.trim(), aliases };
  const currentNetworks = React.useRef(new Set<string>());
  currentNetworks.current = new Set((resource.data ?? []).map(resourceReference));

  const requireCurrent = (id: string) => {
    if (currentNetworks.current.has(id)) return true;
    setError(new Error(`Network ${id} changed or disappeared; inspect and confirm again.`));
    return false;
  };
  const create = async () => {
    const requested = name.trim();
    if (!requested || creation.state === 'loading') return;
    setCreation({ state: 'loading', name: requested, error: null });
    try {
      await api.networks.create(requested);
      await resource.reload();
      setName('');
      setCreation({ state: 'success', name: requested, error: null });
    } catch (cause) {
      setCreation({ state: 'error', name: requested, error: cause });
    }
  };
  const remove = async (network: NetworkSummary) => {
    const id = resourceReference(network);
    setRemovalNotice('');
    if (!requireCurrent(id)) return;
    const removed = await api.networks.removeAndWait(id);
    if (inspection.id === id) setInspection(EMPTY_INSPECTION);
    await resource.reload();
    setRemovalNotice(
      removed.changed
        ? `Network ${id} was removed and its absence was verified.`
        : `Network ${id} removal was accepted, but absence was not observed before the timeout.`,
    );
  };
  const inspect = async (network: NetworkSummary) => {
    const id = resourceReference(network);
    const revision = ++inspectionRevision.current;
    setInspection({ id, state: 'loading', detail: null, error: null });
    try {
      const detail = await api.networks.inspect(id);
      if (revision !== inspectionRevision.current) return;
      setInspection({ id, state: 'ready', detail: detail.id ? detail : null, error: null });
    } catch (cause) {
      if (revision === inspectionRevision.current) {
        setInspection({ id, state: 'error', detail: null, error: cause });
      }
    }
  };
  React.useEffect(() => {
    if (inventoryRevision.current === resource.data) return;
    inventoryRevision.current = resource.data;
    inspectionRevision.current += 1;
    setInspection(EMPTY_INSPECTION);
    setDisconnectRequest(null);
  }, [resource.data]);

  const request = (network: NetworkSummary, verb: EndpointRequest['verb']): EndpointRequest => {
    const containerId = container.trim();
    if (!immutableContainerId(containerId)) {
      throw new TypeError(
        'Enter the complete 32- or 64-character lowercase hexadecimal container ID returned by inspection.',
      );
    }
    return {
      verb,
      network: resourceReference(network),
      container: containerId,
      aliases: verb === 'connect' ? endpointAliases(aliases) : [],
    };
  };
  const attach = async (next: EndpointRequest) => {
    if (
      next.container !== endpointInput.current.container ||
      (next.verb === 'connect' &&
        next.aliases.join(',') !== endpointAliases(endpointInput.current.aliases).join(','))
    ) {
      throw new Error('Endpoint input changed; review and confirm the operation again.');
    }
    if (!requireCurrent(next.network)) {
      throw new Error(`Network ${next.network} changed or disappeared; inspect and confirm again.`);
    }
    setOperation({ state: 'loading', request: next, error: null });
    try {
      if (next.verb === 'connect') {
        await api.networks.connect(next.network, next.container, { aliases: next.aliases });
      } else {
        await api.networks.disconnect(next.network, next.container);
      }
      await resource.reload();
      setOperation({ state: 'success', request: next, error: null });
      setDisconnectRequest(null);
    } catch (cause) {
      setOperation({ state: 'error', request: next, error: cause });
      throw cause;
    }
  };
  const begin = (network: NetworkSummary, verb: EndpointRequest['verb']) => {
    setError(null);
    try {
      const next = request(network, verb);
      if (verb === 'disconnect') setDisconnectRequest(next);
      else void attach(next).catch(() => {});
    } catch (cause) {
      setOperation({ state: 'error', request: null, error: cause });
    }
  };

  const view = bounded(resource.data);
  const inventoryState: 'loading' | 'error' | 'empty' | 'ready' = resource.loading
    ? 'loading'
    : resource.error
      ? 'error'
      : view.records.length === 0
        ? 'empty'
        : 'ready';
  return (
    <Page
      title="Networks"
      subtitle="Bounded network inventory; attachment changes are accepted only for stopped containers."
    >
      <Column gap={1} align="start">
        <Entry
          value={name}
          placeholder="Network name"
          width={{ minimum: { chars: 10 }, maximum: { chars: 32 } }}
          enabled={creation.state !== 'loading'}
          onChange={(event) => {
            setName(String(event.value ?? ''));
            setCreation({ state: 'idle', name: '', error: null });
          }}
        />
        <Row gap={1} wrap>
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
            enabled={creation.state !== 'loading' && name.trim().length > 0}
            onInvoke={() => void create()}
          />
          <Button
            label="Refresh"
            variant="outline"
            enabled={creation.state !== 'loading'}
            onInvoke={resource.reload}
          />
        </Row>
      </Column>
      {creation.state === 'loading' ? (
        <Row gap={1} align="center">
          <Spinner />
          <Text label={`Creating network ${creation.name}…`} />
        </Row>
      ) : null}
      {creation.state === 'error' ? (
        <RecoveryState operation="Creating network" error={creation.error} />
      ) : null}
      {creation.state === 'success' ? (
        <Text label={`Created network ${creation.name}.`} color="positive" wrap />
      ) : null}
      {removalNotice ? <Text label={removalNotice} color="positive" wrap /> : null}
      <Card justify="start" variant="outline">
        <CardContent gap={1}>
          <Text label="Container attachment" />
          <Text label="Required · complete immutable container ID" color="text-dim" />
          <Entry
            value={container}
            placeholder="Complete container ID"
            width={{ minimum: { chars: 10 }, maximum: { chars: 38 } }}
            enabled={operation.state !== 'loading'}
            onChange={(event) => {
              setContainer(String(event.value ?? ''));
              setOperation({ state: 'idle', request: null, error: null });
              setDisconnectRequest(null);
            }}
          />
          <Column gap={1} align="start">
            <Text label="Optional aliases" color="text-dim" />
            <Entry
              value={aliases}
              placeholder="Endpoint aliases (comma-separated, optional)"
              width={{ minimum: { chars: 10 }, maximum: { chars: 38 } }}
              enabled={operation.state !== 'loading'}
              onChange={(event) => {
                setAliases(String(event.value ?? ''));
                setOperation({ state: 'idle', request: null, error: null });
              }}
            />
          </Column>
        </CardContent>
      </Card>
      <OperationStatus operation={operation} onRetry={attach} />
      <ErrorText error={error} />
      <Column grow={false} justify="stretch">
        {inventoryState === 'empty' ? (
          <Column gap={1} align="start" justify="start">
            <Text label="No networks" />
            <Text
              label="Create a network above to connect workspace containers."
              color="text-dim"
              wrap
            />
          </Column>
        ) : (
          <ResourceState
            state={inventoryState}
            loadingLabel="Reading networks…"
            emptyLabel="No networks"
            emptyDetail="Create a network above to connect workspace containers."
            error={boundedMessage(resource.error)}
            retryLabel="Retry networks"
            onRetry={resource.reload}
          >
            {view.records.map((network) => {
              const id = resourceReference(network);
              const membership =
                inspection.id === id && inspection.state === 'ready'
                  ? inspection.detail?.endpoints
                  : network.endpoints;
              const containerId = container.trim();
              const validContainer = immutableContainerId(containerId);
              const membershipUnknown = validContainer && (!membership || membership.truncated);
              const endpointAction = validContainer
                ? membership?.containers.includes(containerId)
                  ? 'disconnect'
                  : membership && !membership.truncated
                    ? 'connect'
                    : null
                : null;
              return (
                <Card
                  key={id}
                  justify="start"
                  variant={inspection.id === id ? 'filled' : 'outline'}
                >
                  <CardHeader
                    label={network.name}
                    detail={`${network.driver} · ${network.scope}`}
                    align="start"
                    width="fill"
                  />
                  {network.kind === 'builtin' ? (
                    <CardContent gap={1}>
                      <Badge label="Built-in · protected" tone="accent" />
                    </CardContent>
                  ) : null}
                  {membershipUnknown ? (
                    <CardContent gap={1}>
                      <Text
                        label="Attachment status unknown for this container · Inspect to resolve"
                        color="warning"
                        wrap
                      />
                    </CardContent>
                  ) : null}
                  <CardActions gap={1} justify="start">
                    <Button
                      label={
                        inspection.id === id && inspection.state === 'error'
                          ? 'Retry inspect'
                          : 'Inspect'
                      }
                      onInvoke={() => inspect(network)}
                    />
                    {endpointAction === 'connect' ? (
                      <Button
                        label="Connect"
                        enabled={operation.state !== 'loading'}
                        onInvoke={() => begin(network, 'connect')}
                      />
                    ) : null}
                    {endpointAction === 'disconnect' ? (
                      <Button
                        label="Disconnect"
                        enabled={operation.state !== 'loading'}
                        tone="danger"
                        onInvoke={() => begin(network, 'disconnect')}
                      />
                    ) : null}
                    {network.kind !== 'builtin' ? (
                      <ConfirmAction
                        authorityKey={`network:${id}:remove`}
                        label="Remove"
                        confirmLabel="Confirm remove"
                        pendingLabel="Confirm remove"
                        question={`Remove immutable network ${id} (${network.name})?`}
                        onConfirm={() => remove(network)}
                      />
                    ) : null}
                  </CardActions>
                  {disconnectRequest?.network === id ? (
                    <DisconnectConsent
                      request={disconnectRequest}
                      loading={operation.state === 'loading'}
                      onConfirm={attach}
                      onCancel={() => setDisconnectRequest(null)}
                    />
                  ) : null}
                  {inspection.id === id ? <NetworkDetail inspection={inspection} /> : null}
                </Card>
              );
            })}
            <Omitted count={view.omitted} />
          </ResourceState>
        )}
      </Column>
    </Page>
  );
}

function OperationStatus({
  operation,
  onRetry,
}: {
  operation: Operation;
  onRetry: (request: EndpointRequest) => Promise<void>;
}) {
  const request = operation.request;
  if (operation.state === 'loading' && request)
    return (
      <Row gap={1} align="center">
        <Spinner />
        <Text label={`${title(request.verb)}ing immutable endpoint…`} />
      </Row>
    );
  if (operation.state === 'error')
    return (
      <Row gap={1} wrap>
        <Text label={boundedMessage(operation.error)} color="danger" wrap />
        {request ? (
          <Button
            label={`Retry ${request.verb}`}
            onInvoke={() => void onRetry(request).catch(() => {})}
          />
        ) : null}
      </Row>
    );
  if (operation.state === 'success' && request)
    return (
      <Text
        label={`${request.verb === 'connect' ? 'Connected' : 'Disconnected'} container ${request.container} ${request.verb === 'connect' ? 'to' : 'from'} network ${request.network}${request.aliases.length ? ` with ${request.aliases.length} endpoint alias${request.aliases.length === 1 ? '' : 'es'}` : ''}.`}
        color="positive"
        wrap
      />
    );
  return null;
}

function DisconnectConsent({
  request,
  loading,
  onConfirm,
  onCancel,
}: {
  request: EndpointRequest;
  loading: boolean;
  onConfirm: (request: EndpointRequest) => Promise<void>;
  onCancel: () => void;
}) {
  return (
    <CardContent>
      <Text
        label={`Disconnect immutable container ${request.container} from network ${request.network}?`}
        color="warning"
        wrap
      />
      <Row gap={1}>
        <Button
          label="Confirm disconnect"
          enabled={!loading}
          tone="danger"
          destructive
          onInvoke={() => void onConfirm(request).catch(() => {})}
        />
        <Button label="Cancel" enabled={!loading} onInvoke={onCancel} />
      </Row>
    </CardContent>
  );
}

function NetworkDetail({ inspection }: { inspection: Inspection }) {
  return (
    <CardContent>
      {inspection.state === 'loading' ? (
        <Row gap={1} align="center">
          <Spinner />
          <Text label="Reading network details…" />
        </Row>
      ) : inspection.state === 'error' ? (
        <Text label={boundedMessage(inspection.error)} color="danger" wrap />
      ) : !inspection.detail ? (
        <EmptyState label="No network details" detail="The host returned no inspectable fields." />
      ) : (
        <NetworkSummaryDetail network={inspection.detail} />
      )}
    </CardContent>
  );
}

function NetworkSummaryDetail({ network }: { network: NetworkSummary }) {
  const endpoints = network.endpoints;
  const containers = endpoints?.containers ?? [];
  return (
    <Column gap={1}>
      <Heading label="Network details" scale="caption" />
      <Row gap={1} wrap>
        <Badge label={`Driver · ${network.driver}`} />
        <Badge label={`Scope · ${network.scope}`} />
        <Badge label={network.kind === 'builtin' ? 'Built-in' : 'Custom'} />
      </Row>
      <Text label={`Immutable network ID · ${network.id}`} color="text-dim" wrap />
      <Heading label={`Connected containers · ${containers.length}`} scale="caption" />
      {!endpoints ? (
        <InlineMessage
          label="Endpoint membership was not included in this inspection."
          tone="warning"
        />
      ) : containers.length === 0 ? (
        <EmptyState label="No connected containers" detail="This network has no endpoints." />
      ) : (
        containers.map((container) => (
          <Text key={container} label={`Container · ${container}`} wrap />
        ))
      )}
      {endpoints?.truncated ? (
        <InlineMessage
          label="Additional connected containers were omitted by the host safety limit."
          tone="warning"
        />
      ) : null}
    </Column>
  );
}

function Page({
  title: label,
  subtitle,
  children,
}: {
  title: string;
  subtitle: string;
  children: React.ReactNode;
}) {
  return (
    <Scroll grow height="fill">
      <Column width="fill" pad={4} gap={2}>
        <Heading label={label} scale="title" />
        <Text label={subtitle} color="text-dim" wrap />
        {children}
      </Column>
    </Scroll>
  );
}
function ErrorText({ error }: { error: unknown }) {
  return error ? <Text label={boundedMessage(error)} color="danger" wrap /> : null;
}
function Omitted({ count }: { count: number }) {
  return count > 0 ? (
    <Text label={`${count} more records omitted to keep this view bounded.`} color="text-dim" />
  ) : null;
}
function title(value: string) {
  return value.charAt(0).toUpperCase() + value.slice(1);
}
