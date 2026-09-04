import React from 'react';
import {
  Button, Card, CardActions, CardContent, CardHeader, Column, ConfirmAction, EmptyState, Entry,
  Heading, ObjectInspector, ResourceState, Row, Scroll, Spinner, Text,
  type NetworkSummary, type WorkspaceApi,
} from '@husklet/react';
import {
  NetworkDetailsSource, bounded, boundedMessage, endpointAliases, immutableContainerId, resourceReference,
} from './model.js';
import type { Resource } from './overview.js';

const INSPECTOR_BOUNDS = Object.freeze({ maxDepth: 8, maxNodes: 128, maxStringLength: 256 });
type Inspection = {
  id: string; state: 'idle' | 'loading' | 'ready' | 'error'; count: number;
  detail: NetworkSummary | null; error: unknown;
};
type Creation = { state: 'idle' | 'loading' | 'success' | 'error'; name: string; error: unknown };
type EndpointRequest = {
  verb: 'connect' | 'disconnect'; network: string; container: string; aliases: string[];
};
type Operation = {
  state: 'idle' | 'loading' | 'success' | 'error'; request: EndpointRequest | null; error: unknown;
};
const EMPTY_INSPECTION: Inspection = { id: '', state: 'idle', count: 0, detail: null, error: null };

export function Networks({ api, resource, networkDetails }: {
  api: WorkspaceApi;
  resource: Resource<NetworkSummary>;
  networkDetails?: NetworkDetailsSource;
}) {
  const localDetails = React.useMemo(() => new NetworkDetailsSource(), []);
  const detailsSource = networkDetails ?? localDetails;
  const [name, setName] = React.useState('');
  const [container, setContainer] = React.useState('');
  const [aliases, setAliases] = React.useState('');
  const [inspection, setInspection] = React.useState<Inspection>(EMPTY_INSPECTION);
  const [error, setError] = React.useState<unknown>(null);
  const [creation, setCreation] = React.useState<Creation>({ state: 'idle', name: '', error: null });
  const [operation, setOperation] = React.useState<Operation>({ state: 'idle', request: null, error: null });
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
    if (!requireCurrent(id)) return;
    await api.networks.remove(id);
    if (inspection.id === id) setInspection(EMPTY_INSPECTION);
    await resource.reload();
  };
  const inspect = async (network: NetworkSummary) => {
    const id = resourceReference(network);
    const revision = ++inspectionRevision.current;
    setInspection({ id, state: 'loading', count: 0, detail: null, error: null });
    try {
      const detail = await api.networks.inspect(id);
      if (revision !== inspectionRevision.current) return;
      const count = await detailsSource.replace(detail);
      if (revision !== inspectionRevision.current) return;
      setInspection({ id, state: 'ready', count, detail, error: null });
    } catch (cause) {
      if (revision === inspectionRevision.current) {
        setInspection({ id, state: 'error', count: 0, detail: null, error: cause });
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
      throw new TypeError('Enter the complete 32- or 64-character lowercase hexadecimal container ID returned by inspection.');
    }
    return {
      verb, network: resourceReference(network), container: containerId,
      aliases: verb === 'connect' ? endpointAliases(aliases) : [],
    };
  };
  const attach = async (next: EndpointRequest) => {
    if (next.container !== endpointInput.current.container
      || (next.verb === 'connect'
        && next.aliases.join(',') !== endpointAliases(endpointInput.current.aliases).join(','))) {
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
    ? 'loading' : resource.error ? 'error' : view.records.length === 0 ? 'empty' : 'ready';
  return <Page title="Networks"
    subtitle="Bounded network inventory; attachment changes are accepted only for stopped containers.">
    <Row gap={1}>
      <Entry value={name} placeholder="Network name" enabled={creation.state !== 'loading'}
        onChange={(event) => {
          setName(String(event.value ?? ''));
          setCreation({ state: 'idle', name: '', error: null });
        }} />
      <Button label={creation.state === 'loading' ? 'Creating…' : creation.state === 'error' ? 'Retry create' : 'Create'}
        enabled={creation.state !== 'loading' && name.trim().length > 0} onInvoke={() => void create()} />
      <Button label="Refresh" enabled={creation.state !== 'loading'} onInvoke={resource.reload} />
    </Row>
    {creation.state === 'loading' ? <Row gap={1} align="center">
      <Spinner /><Text label={`Creating network ${creation.name}…`} />
    </Row> : null}
    {creation.state === 'error' ? <Text label={boundedMessage(creation.error)} color="danger" wrap /> : null}
    {creation.state === 'success' ? <Text label={`Created network ${creation.name}.`} color="positive" wrap /> : null}
    <Entry value={container} placeholder="Complete container ID" enabled={operation.state !== 'loading'}
      onChange={(event) => {
        setContainer(String(event.value ?? ''));
        setOperation({ state: 'idle', request: null, error: null });
        setDisconnectRequest(null);
      }} />
    <Entry value={aliases} placeholder="Endpoint aliases (comma-separated, optional)"
      enabled={operation.state !== 'loading'} onChange={(event) => {
        setAliases(String(event.value ?? ''));
        setOperation({ state: 'idle', request: null, error: null });
      }} />
    <OperationStatus operation={operation} onRetry={attach} />
    <ErrorText error={error} />
    <ResourceState state={inventoryState} loadingLabel="Reading networks…" emptyLabel="No networks"
      emptyDetail="Create a network above to connect workspace containers."
      error={boundedMessage(resource.error)} retryLabel="Retry networks" onRetry={resource.reload}>
      {view.records.map((network) => {
        const id = resourceReference(network);
        return <Card key={id} variant={inspection.id === id ? 'filled' : 'outline'}>
          <CardHeader label={network.name} detail={`${network.driver} · ${network.scope}`} />
          <CardActions gap={1}>
            <Button label={inspection.id === id && inspection.state === 'error' ? 'Retry inspect' : 'Inspect'}
              onInvoke={() => inspect(network)} />
            <Button label="Connect" enabled={operation.state !== 'loading' && container.trim().length > 0}
              onInvoke={() => begin(network, 'connect')} />
            <Button label="Disconnect" enabled={operation.state !== 'loading' && container.trim().length > 0}
              tone="danger" onInvoke={() => begin(network, 'disconnect')} />
            <ConfirmAction authorityKey={`network:${id}:remove`} label="Remove" confirmLabel="Confirm remove"
              pendingLabel="Confirm remove" question={`Remove immutable network ${id} (${network.name})?`}
              onConfirm={() => remove(network)} />
          </CardActions>
          {disconnectRequest?.network === id ? <DisconnectConsent request={disconnectRequest}
            loading={operation.state === 'loading'} onConfirm={attach} onCancel={() => setDisconnectRequest(null)} /> : null}
          {inspection.id === id ? <NetworkDetail inspection={inspection} /> : null}
        </Card>;
      })}
      <Omitted count={view.omitted} />
    </ResourceState>
  </Page>;
}

function OperationStatus({ operation, onRetry }: { operation: Operation; onRetry: (request: EndpointRequest) => Promise<void> }) {
  const request = operation.request;
  if (operation.state === 'loading' && request) return <Row gap={1} align="center">
    <Spinner /><Text label={`${title(request.verb)}ing immutable endpoint…`} />
  </Row>;
  if (operation.state === 'error') return <Row gap={1} wrap>
    <Text label={boundedMessage(operation.error)} color="danger" wrap />
    {request ? <Button label={`Retry ${request.verb}`} onInvoke={() => void onRetry(request).catch(() => {})} /> : null}
  </Row>;
  if (operation.state === 'success' && request) return <Text
    label={`${request.verb === 'connect' ? 'Connected' : 'Disconnected'} container ${request.container} ${request.verb === 'connect' ? 'to' : 'from'} network ${request.network}${request.aliases.length ? ` with ${request.aliases.length} endpoint alias${request.aliases.length === 1 ? '' : 'es'}` : ''}.`}
    color="positive" wrap />;
  return null;
}

function DisconnectConsent({ request, loading, onConfirm, onCancel }: {
  request: EndpointRequest; loading: boolean; onConfirm: (request: EndpointRequest) => Promise<void>; onCancel: () => void;
}) {
  return <CardContent>
    <Text label={`Disconnect immutable container ${request.container} from network ${request.network}?`} color="warning" wrap />
    <Row gap={1}>
      <Button label="Confirm disconnect" enabled={!loading} tone="danger" destructive
        onInvoke={() => void onConfirm(request).catch(() => {})} />
      <Button label="Cancel" enabled={!loading} onInvoke={onCancel} />
    </Row>
  </CardContent>;
}

function NetworkDetail({ inspection }: { inspection: Inspection }) {
  return <CardContent>{inspection.state === 'loading' ? <Row gap={1} align="center">
    <Spinner /><Text label="Reading network details…" />
  </Row> : inspection.state === 'error'
    ? <Text label={boundedMessage(inspection.error)} color="danger" wrap />
    : inspection.count === 0
      ? <EmptyState label="No network details" detail="The host returned no inspectable fields." />
      : <ObjectInspector value={inspection.detail} {...INSPECTOR_BOUNDS}
        height={{ minimum: { step: 10 }, maximum: { step: 32 } }} />}</CardContent>;
}

function Page({ title: label, subtitle, children }: { title: string; subtitle: string; children: React.ReactNode }) {
  return <Scroll grow height="fill"><Column pad={4} gap={2}>
    <Heading label={label} scale="title" /><Text label={subtitle} color="text-dim" wrap />{children}
  </Column></Scroll>;
}
function ErrorText({ error }: { error: unknown }) { return error ? <Text label={boundedMessage(error)} color="danger" wrap /> : null; }
function Omitted({ count }: { count: number }) { return count > 0 ? <Text label={`${count} more records omitted to keep this view bounded.`} color="text-dim" /> : null; }
function title(value: string) { return value.charAt(0).toUpperCase() + value.slice(1); }
