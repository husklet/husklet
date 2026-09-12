import React from 'react';
import {
  Badge,
  Button,
  Card,
  CardContent,
  Column,
  Container,
  ConfirmAction,
  EmptyState,
  Entry,
  Expander,
  FormHelperText,
  FormLabel,
  Heading,
  IconButton,
  InlineMessage,
  InlineButton,
  ResourceState,
  RecoveryState,
  ResourceIdentity,
  Row,
  Scroll,
  Select,
  Spinner,
  Text,
  type ContainerSummary,
  type NetworkSummary,
  type WorkspaceApi,
} from '@husklet/react';
import {
  bounded,
  boundedMessage,
  endpointAliases,
  immutableContainerId,
  resourceReference,
  shortId,
} from './model.js';
import type { Resource } from './overview.js';
import { ResourceSummary } from './resource-summary.js';
import { AuthorityRecovery } from './authority-recovery.js';

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
  networkName: string;
  container: string;
  containerName: string;
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
  containers,
  onOpenExtensions,
}: {
  api: WorkspaceApi;
  resource: Resource<NetworkSummary>;
  containers: Resource<ContainerSummary>;
  onOpenExtensions: () => void;
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
  const verificationNetwork = React.useRef('');
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
    verificationNetwork.current = '';
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
  const toggleInspection = (network: NetworkSummary) => {
    const id = resourceReference(network);
    if (inspection.id === id && inspection.state === 'ready') {
      inspectionRevision.current += 1;
      verificationNetwork.current = '';
      setInspection(EMPTY_INSPECTION);
      setDisconnectRequest(null);
      setOperation({ state: 'idle', request: null, error: null });
      return;
    }
    void inspect(network);
  };
  React.useEffect(() => {
    if (inventoryRevision.current === resource.data) return;
    inventoryRevision.current = resource.data;
    const verifying = verificationNetwork.current;
    if (verifying) {
      if ((resource.data ?? []).some((network) => resourceReference(network) === verifying)) return;
      verificationNetwork.current = '';
      inspectionRevision.current += 1;
      setInspection({
        id: verifying,
        state: 'error',
        detail: null,
        error: new Error(`Network ${verifying} changed or disappeared after the operation.`),
      });
      setDisconnectRequest(null);
      return;
    }
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
      networkName: network.name || shortId(resourceReference(network)),
      container: containerId,
      containerName:
        (containers.data ?? []).find((candidate) => candidate.id === containerId)?.name ||
        shortId(containerId),
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
      setOperation({ state: 'success', request: next, error: null });
      verificationNetwork.current = next.network;
      await resource.reload();
      setDisconnectRequest(null);
      const revision = ++inspectionRevision.current;
      setInspection((current) => ({
        id: next.network,
        state: 'loading',
        detail: current.id === next.network ? current.detail : null,
        error: null,
      }));
      try {
        const detail = await api.networks.inspect(next.network);
        if (revision === inspectionRevision.current) {
          setInspection({
            id: next.network,
            state: 'ready',
            detail: detail.id ? detail : null,
            error: null,
          });
        }
      } catch (cause) {
        if (revision === inspectionRevision.current) {
          setInspection({ id: next.network, state: 'error', detail: null, error: cause });
        }
      }
    } catch (cause) {
      verificationNetwork.current = '';
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
  const containerChoices = (containers?.data ?? [])
    .filter(
      (candidate) =>
        immutableContainerId(candidate.id) && ['created', 'exited'].includes(candidate.state),
    )
    .map((candidate) => ({
      value: candidate.id,
      label: `${candidate.name || 'Unnamed container'} · ${shortId(candidate.id)} · ${candidate.state}`,
    }));
  React.useEffect(() => {
    if (!container) return;
    if (containerChoices.some((choice) => choice.value === container)) return;
    setContainer('');
    setOperation({ state: 'idle', request: null, error: null });
    setDisconnectRequest(null);
  }, [container, containerChoices, containers]);
  const inventoryState: 'loading' | 'error' | 'empty' | 'ready' = resource.loading
    ? resource.error
      ? 'error'
      : 'loading'
    : resource.error
      ? 'error'
      : view.records.length === 0
        ? 'empty'
        : 'ready';
  return (
    <Page title="Networks" subtitle="Create networks and connect stopped workspace containers.">
      <Column gap={1} width={{ minimum: { chars: 20 }, maximum: { chars: 64 } }}>
        <FormLabel label="Create a network" />
        <Row gap={1} wrap width="fill" align="center" justify="start">
          <Entry
            value={name}
            placeholder="Network name"
            width={{ minimum: { chars: 20 }, maximum: { chars: 40 } }}
            grow
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
            enabled={creation.state !== 'loading' && name.trim().length > 0}
            onInvoke={() => void create()}
          />
          <IconButton
            label="Refresh"
            tooltip="Refresh networks"
            icon="view-refresh-symbolic"
            size="large"
            variant="ghost"
            enabled={creation.state !== 'loading'}
            onInvoke={resource.reload}
          />
        </Row>
        <FormHelperText label="Use a short name that describes what the connected services share." />
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
      <ErrorText error={error} />
      <Column width="fill" grow={false} justify="stretch">
        {inventoryState === 'empty' ? (
          <Column gap={1} align="start" justify="start">
            <Text label="No networks" />
            <Text
              label="Create a network above to connect workspace containers."
              color="text-dim"
              wrap
            />
          </Column>
        ) : inventoryState === 'error' ? (
          <NetworkInventoryFailure
            error={resource.error}
            retrying={resource.loading}
            onRetry={resource.reload}
            onOpenExtensions={onOpenExtensions}
          />
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
                  ? (inspection.detail?.endpoints ?? network.endpoints)
                  : network.endpoints;
              const containerId = container.trim();
              const validContainer = immutableContainerId(containerId);
              const membershipUnknown =
                inspection.id === id && validContainer && (!membership || membership.truncated);
              const endpointAction = validContainer
                ? membership?.containers.includes(containerId)
                  ? 'disconnect'
                  : membership && !membership.truncated
                    ? 'connect'
                    : null
                : null;
              const inspectionNeedsAccess =
                inspection.id === id &&
                inspection.state === 'error' &&
                isAuthorityDenial(inspection.error);
              const membershipLabel = !membership
                ? 'Connections unknown'
                : membership.truncated
                  ? `${membership.containers.length} shown · more omitted`
                  : `${membership.containers.length} connected`;
              return (
                <Card key={id} width="fill" variant={inspection.id === id ? 'filled' : 'outline'}>
                  <ResourceSummary
                    label={network.name}
                    detail={`${network.driver} · ${network.scope}`}
                    status={
                      <>
                        <Badge
                          label={membershipLabel}
                          tone={!membership || membership.truncated ? 'warning' : 'neutral'}
                        />
                        {network.kind === 'builtin' ? (
                          <Badge label="Built-in · protected" tone="accent" />
                        ) : null}
                      </>
                    }
                    actions={
                      inspectionNeedsAccess ? null : (
                        <>
                          <InlineButton
                            key={`manage-${id}`}
                            label={
                              inspection.id !== id
                                ? 'Manage connections'
                                : inspection.state === 'loading'
                                  ? 'Managing connections…'
                                  : inspection.state === 'error'
                                    ? 'Retry managing connections'
                                    : 'Hide connections'
                            }
                            variant={inspection.id === id ? 'filled' : 'outline'}
                            tone={inspection.id === id ? 'accent' : 'neutral'}
                            enabled={inspection.state !== 'loading'}
                            onInvoke={() => toggleInspection(network)}
                          />
                          {inspection.id === id && inspection.state === 'ready' ? (
                            <IconButton
                              label="Refresh connections"
                              tooltip="Refresh network details"
                              icon="view-refresh-symbolic"
                              size="small"
                              variant="ghost"
                              onInvoke={() => void inspect(network)}
                            />
                          ) : null}
                        </>
                      )
                    }
                    overflow={
                      network.kind !== 'builtin' ? (
                        <Expander
                          label="Danger zone"
                          variant="outline"
                          width="content"
                          align="start"
                          tooltip="Remove this network from the workspace"
                        >
                          <Column gap={1}>
                            <Text
                              label="Removing this network disconnects it from the workspace and cannot be undone."
                              color="text-dim"
                              wrap
                            />
                            <Row>
                              <ConfirmAction
                                authorityKey={`network:${id}:remove`}
                                label="Remove"
                                confirmLabel="Confirm remove"
                                pendingLabel="Confirm remove"
                                question={`Remove immutable network ${id} (${network.name})?`}
                                size="small"
                                onConfirm={() => remove(network)}
                              />
                            </Row>
                          </Column>
                        </Expander>
                      ) : null
                    }
                  />
                  {membershipUnknown ? (
                    <CardContent>
                      {membershipUnknown ? (
                        <Text
                          label="Attachment status unknown for this container · Manage connections to resolve"
                          color="warning"
                          wrap
                        />
                      ) : null}
                    </CardContent>
                  ) : null}
                  {disconnectRequest?.network === id ? (
                    <DisconnectConsent
                      key="disconnect-consent"
                      request={disconnectRequest}
                      loading={operation.state === 'loading'}
                      onConfirm={attach}
                      onCancel={() => setDisconnectRequest(null)}
                    />
                  ) : null}
                  <Column key="connection-management" gap={0} width="fill">
                    {inspection.id === id ? (
                      <>
                        <NetworkDetail
                          inspection={inspection}
                          onOpenExtensions={onOpenExtensions}
                        />
                        {inspection.state === 'ready' ? (
                          <CardContent gap={1}>
                            <Heading label="Container attachment" scale="caption" />
                            <Text
                              label="Connect or disconnect one inspected container from this network."
                              color="text-dim"
                              wrap
                            />
                            {containerChoices.length ? (
                              <Row gap={1} align="center" wrap width="fill">
                                <Select
                                  value={container}
                                  choices={containerChoices}
                                  tooltip={
                                    container || 'Choose a container by name and immutable ID'
                                  }
                                  width={{ minimum: { chars: 10 }, maximum: { chars: 38 } }}
                                  enabled={operation.state !== 'loading'}
                                  onChange={(event) => {
                                    setContainer(String(event.value ?? ''));
                                    setOperation({ state: 'idle', request: null, error: null });
                                    setDisconnectRequest(null);
                                  }}
                                />
                                {endpointAction === 'connect' ? (
                                  <Button
                                    label="Connect"
                                    variant="filled"
                                    tone="accent"
                                    enabled={operation.state !== 'loading'}
                                    onInvoke={() => begin(network, 'connect')}
                                  />
                                ) : null}
                                {endpointAction === 'disconnect' ? (
                                  <Button
                                    label="Disconnect"
                                    variant="outline"
                                    tone="danger"
                                    enabled={operation.state !== 'loading'}
                                    onInvoke={() => begin(network, 'disconnect')}
                                  />
                                ) : null}
                              </Row>
                            ) : (
                              <InlineMessage
                                label={
                                  containers.loading
                                    ? 'Reading containers…'
                                    : 'No containers are available to attach.'
                                }
                                tone="neutral"
                              />
                            )}
                            {endpointAction === 'connect' ? (
                              <Entry
                                value={aliases}
                                placeholder="Aliases, comma-separated (optional)"
                                width={{ minimum: { chars: 10 }, maximum: { chars: 38 } }}
                                enabled={operation.state !== 'loading'}
                                onChange={(event) => {
                                  setAliases(String(event.value ?? ''));
                                  setOperation({ state: 'idle', request: null, error: null });
                                }}
                              />
                            ) : null}
                            <OperationStatus operation={operation} onRetry={attach} />
                          </CardContent>
                        ) : null}
                      </>
                    ) : null}
                  </Column>
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

type FailureKind = 'denied' | 'unavailable' | 'absent' | 'conflict' | 'unsupported' | 'failed';

function failureKind(error: unknown): FailureKind {
  if (!error || typeof error !== 'object') return 'failed';
  const kind = (error as { kind?: unknown }).kind;
  return ['denied', 'unavailable', 'absent', 'conflict', 'unsupported', 'failed'].includes(
    String(kind),
  )
    ? (kind as FailureKind)
    : 'failed';
}

function NetworkInventoryFailure({
  error,
  retrying,
  onRetry,
  onOpenExtensions,
}: {
  error: unknown;
  retrying: boolean;
  onRetry: () => void;
  onOpenExtensions: () => void;
}) {
  const kind = failureKind(error);
  const diagnostic = boundedMessage(error);
  const denied = kind === 'denied';
  const changed = kind === 'absent' || kind === 'conflict';
  const unsupported = kind === 'unsupported';
  const label = denied
    ? 'Top does not have permission to list networks. Review its network access in Extensions.'
    : kind === 'unavailable'
      ? 'Network inventory is unavailable. Check that the workspace is running, then retry.'
      : changed
        ? 'Network inventory changed or is no longer available. Refresh to load current records.'
        : unsupported
          ? 'Network inventory is not supported by this workspace.'
          : 'Network inventory could not be loaded. Retry, then inspect technical details if it continues.';
  return (
    <Column gap={1} align="start" width="fill">
      <InlineMessage label={label} tone={denied || unsupported ? 'warning' : 'danger'} />
      {!unsupported ? (
        <Button
          label={
            denied
              ? 'Review access'
              : retrying
                ? 'Retrying networks…'
                : changed
                  ? 'Refresh networks'
                  : 'Retry networks'
          }
          variant={denied ? 'filled' : 'outline'}
          tone="accent"
          size="small"
          enabled={!retrying}
          onInvoke={denied ? onOpenExtensions : onRetry}
        />
      ) : null}
      {diagnostic ? (
        <Expander label="Technical details">
          <Text label={diagnostic} wrap />
        </Expander>
      ) : null}
    </Column>
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
      <Column gap={1} align="start" width="fill">
        <InlineMessage
          label={`${request.verb === 'connect' ? 'Connected' : 'Disconnected'} ${request.containerName} ${request.verb === 'connect' ? 'to' : 'from'} ${request.networkName}`}
          tone="positive"
          width="fill"
        />
        <Row gap={1} wrap>
          <Badge label={`Container · ${shortId(request.container)}`} />
          <Badge label={`Network · ${shortId(request.network)}`} />
          {request.aliases.length ? (
            <Badge
              label={`${request.aliases.length} endpoint alias${request.aliases.length === 1 ? '' : 'es'}`}
            />
          ) : null}
        </Row>
        <Expander label="Technical details">
          <Column gap={1}>
            <Text label={`Container ID · ${request.container}`} wrap />
            <Text label={`Network ID · ${request.network}`} wrap />
          </Column>
        </Expander>
      </Column>
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

function NetworkDetail({
  inspection,
  onOpenExtensions,
}: {
  inspection: Inspection;
  onOpenExtensions: () => void;
}) {
  return (
    <CardContent>
      {inspection.state === 'loading' ? (
        <Row gap={1} align="center">
          <Spinner />
          <Text label="Reading network details…" />
        </Row>
      ) : inspection.state === 'error' ? (
        isAuthorityDenial(inspection.error) ? (
          <AuthorityRecovery resource="network" onOpenExtensions={onOpenExtensions} />
        ) : (
          <Text label={boundedMessage(inspection.error)} color="danger" wrap />
        )
      ) : !inspection.detail ? (
        <EmptyState label="No network details" detail="The host returned no inspectable fields." />
      ) : (
        <NetworkSummaryDetail network={inspection.detail} />
      )}
    </CardContent>
  );
}

function isAuthorityDenial(error: unknown): boolean {
  if (!error || typeof error !== 'object') return false;
  const failure = error as { kind?: unknown; capability?: unknown; message?: unknown };
  return (
    failure.kind === 'denied' ||
    (typeof failure.message === 'string' && failure.message.includes('consented resource scope'))
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
      <ResourceIdentity label="Immutable network ID" value={network.id} />
      <Heading label={`Connected containers · ${containers.length}`} scale="caption" />
      {!endpoints ? (
        <InlineMessage
          label="Endpoint membership was not included in this inspection."
          tone="warning"
        />
      ) : containers.length === 0 ? (
        <Text label="No containers connected." color="text-dim" />
      ) : (
        containers.map((container) => (
          <Text key={container} label={`Container · ${shortId(container)}`} tooltip={container} />
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
    <Scroll grow width="fill" height="fill">
      <Column width="fill" pad={4}>
        <Container>
          <Column width="fill" gap={1}>
            <Heading label={label} scale="display" />
            <Text label={subtitle} color="text-dim" wrap />
            {children}
          </Column>
        </Container>
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
