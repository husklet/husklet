import React from 'react';
import {
  Badge,
  Button,
  Card,
  CardContent,
  CardHeader,
  Column,
  ConfirmAction,
  Entry,
  FormControlLabel,
  Heading,
  InlineMessage,
  ResourceState,
  Row,
  Scroll,
  Spinner,
  Switch,
  Text,
  type ExtensionAcquisitionStatus,
  type ExtensionCapability,
  type ExtensionCatalogue,
  type ExtensionSummary,
  type ContainerGrant,
  type ContainerSelector,
  type FilesystemGrant,
  type WorkspaceApi,
} from '@husklet/react';

type Change = { value?: unknown };

const CONTENT_WIDTH = { minimum: { chars: 48 }, maximum: { chars: 72 } } as const;
const FILESYSTEM_VERBS = [
  { key: 'read', label: 'View contents', meaning: 'read' },
  { key: 'write', label: 'Modify existing contents', meaning: 'write' },
  { key: 'create', label: 'Create new entries', meaning: 'create' },
  { key: 'delete', label: 'Delete entries', meaning: 'delete' },
  { key: 'rename', label: 'Rename or move entries', meaning: 'rename' },
] as const;
type FilesystemVerb = (typeof FILESYSTEM_VERBS)[number]['key'];

function emptyFilesystemGrant(): Required<FilesystemGrant> {
  return { read: [], write: [], create: [], delete: [], rename: [] };
}

function filesystemRoots(grant: FilesystemGrant, verb: FilesystemVerb): string[] {
  return grant[verb] ?? [];
}

function filesystemGrantCount(grant: FilesystemGrant): number {
  return FILESYSTEM_VERBS.reduce((count, { key }) => count + filesystemRoots(grant, key).length, 0);
}

function FilesystemConsent({
  requested,
  granted,
  onChange,
}: {
  requested: FilesystemGrant;
  granted: FilesystemGrant;
  onChange: React.Dispatch<React.SetStateAction<FilesystemGrant>>;
}) {
  const requestCount = FILESYSTEM_VERBS.reduce(
    (count, { key }) => count + filesystemRoots(requested, key).length,
    0,
  );
  if (requestCount === 0) return <Text label="No workspace paths requested." color="text-dim" />;

  return (
    <Column gap={1}>
      <Text
        label="Each switch grants only the named action and root. Modify cannot create, delete, or rename."
        color="text-dim"
        wrap
      />
      <Text
        label={`${filesystemGrantCount(granted)}/${requestCount} workspace paths allowed`}
        color="text-dim"
      />
      {FILESYSTEM_VERBS.flatMap(({ key, label, meaning }) =>
        filesystemRoots(requested, key).map((path) => (
          <FormControlLabel
            key={`${key}:${path}`}
            label={`${label} · ${path} (${meaning})`}
            gap={2}
          >
            <Switch
              checked={filesystemRoots(granted, key).includes(path)}
              onToggle={(event: Change) =>
                onChange((current) => {
                  const roots = filesystemRoots(current, key);
                  return {
                    ...current,
                    [key]: event.value
                      ? [...new Set([...roots, path])]
                      : roots.filter((candidate) => candidate !== path),
                  };
                })
              }
            />
          </FormControlLabel>
        )),
      )}
    </Column>
  );
}

export function Extensions({ api }: { api: WorkspaceApi }) {
  const [installed, setInstalled] = React.useState<ExtensionSummary[]>([]);
  const [catalogue, setCatalogue] = React.useState<ExtensionCatalogue | null>(null);
  const [catalogueError, setCatalogueError] = React.useState('');
  const [inventoryState, setInventoryState] = React.useState<
    'loading' | 'empty' | 'error' | 'ready'
  >('loading');
  const [inventoryError, setInventoryError] = React.useState('');
  const [watchError, setWatchError] = React.useState('');
  const [reference, setReference] = React.useState('');
  const [acquisition, setAcquisition] = React.useState<ExtensionAcquisitionStatus | null>(null);
  const [granted, setGranted] = React.useState<ExtensionCapability[]>([]);
  const [grantedContainers, setGrantedContainers] = React.useState<ContainerGrant>({
    selectors: [],
    create: false,
  });
  const [grantedFilesystem, setGrantedFilesystem] =
    React.useState<FilesystemGrant>(emptyFilesystemGrant);
  const [busy, setBusy] = React.useState('');
  const [error, setError] = React.useState('');
  const [notice, setNotice] = React.useState<{ label: string; uncertain: boolean } | null>(null);
  const cancelling = React.useRef(false);
  const cancelledJob = React.useRef('');
  const candidateKey = React.useRef('');
  const inventoryEpoch = React.useRef(0);

  const reload = React.useCallback(async () => {
    const epoch = ++inventoryEpoch.current;
    setInventoryState('loading');
    setInventoryError('');
    try {
      const listing = await api.extensions.list();
      if (inventoryEpoch.current !== epoch) return;
      setInstalled(listing);
      setInventoryState(listing.length === 0 ? 'empty' : 'ready');
    } catch (cause) {
      if (inventoryEpoch.current !== epoch) return;
      setInstalled([]);
      setInventoryError(message(cause));
      setInventoryState('error');
    }
  }, [api]);
  React.useEffect(() => {
    void reload();
  }, [reload]);
  React.useEffect(() => {
    const catalogue = api.extensions.catalogue;
    if (!catalogue) return;
    void catalogue()
      .then((value) => {
        setCatalogue(value);
        setCatalogueError('');
      })
      .catch((cause) => {
        setCatalogue(null);
        setCatalogueError(message(cause));
      });
  }, [api]);
  React.useEffect(() => {
    let dispose: (() => Promise<void>) | undefined;
    void api
      .watchExtensions((listing) => {
        ++inventoryEpoch.current;
        setInstalled(listing);
        setInventoryState(listing.length === 0 ? 'empty' : 'ready');
        setInventoryError('');
      })
      .then((stop) => {
        dispose = stop;
        setWatchError('');
      })
      .catch((cause) =>
        setWatchError(
          `Live extension updates are unavailable: ${message(cause)} Refresh to read the current inventory.`,
        ),
      );
    return () => {
      void dispose?.();
    };
  }, [api]);

  const inspect = async (suggested?: string) => {
    const wanted = (suggested ?? reference).trim();
    if (!wanted || busy) return;
    setReference(wanted);
    setBusy('inspect');
    setError('');
    setNotice(null);
    setAcquisition(null);
    candidateKey.current = '';
    try {
      const started = await api.extensions.startAcquisition(wanted);
      cancelledJob.current = '';
      let status = await api.extensions.acquisition(started.job);
      // Acquisition polling is event-handler work, not render-time computation.
      // eslint-disable-next-line react-hooks/purity
      const deadline = Date.now() + 30_000;
      while (true) {
        setAcquisition(status);
        if (status.candidate) {
          const key = `${status.job}:${status.candidate.image_digest}`;
          if (candidateKey.current !== key) {
            candidateKey.current = key;
            // Every authority is opt-in. Inspection must never grant access,
            // including during an update where a manifest may have widened.
            setGranted([]);
            setGrantedContainers({ selectors: [], create: false });
            setGrantedFilesystem(emptyFilesystemGrant());
          }
        }
        if (
          status.state === 'ready' ||
          status.state === 'failed' ||
          status.state === 'cancelled' ||
          cancelledJob.current === started.job
        )
          break;
        // eslint-disable-next-line react-hooks/purity
        const remaining = deadline - Date.now();
        if (remaining <= 0) break;
        const changed = await api.extensions.waitForAcquisition(started.job, status.revision, {
          timeoutMs: Math.min(1_000, remaining),
        });
        if (changed.changed) status = changed.status;
      }
      if (
        !['ready', 'failed', 'cancelled'].includes(status.state) &&
        cancelledJob.current !== started.job
      )
        setError(
          'Acquisition is still running. You can cancel it or inspect the reference again later.',
        );
    } catch (cause) {
      setError(message(cause));
    } finally {
      setBusy('');
    }
  };
  const publish = async () => {
    if (!acquisition?.candidate || acquisition.state !== 'ready' || busy) return;
    const updating = Boolean(acquisition.candidate.installed_image_digest);
    setBusy(updating ? 'update' : 'install');
    setError('');
    setNotice(null);
    try {
      const result = await api.extensions[updating ? 'updateAndWait' : 'installAndWait'](
        acquisition.job,
        acquisition.revision,
        granted,
        grantedContainers,
        grantedFilesystem,
      );
      setAcquisition(null);
      setReference('');
      await reload();
      setNotice(
        result.changed
          ? {
              label: `${result.extension.name} ${updating ? 'updated' : 'installed'} and verified.`,
              uncertain: false,
            }
          : {
              label: `${updating ? 'Update' : 'Install'} was accepted, but the resulting extension was not observed. Refresh before acting again.`,
              uncertain: true,
            },
      );
    } catch (cause) {
      setError(message(cause));
    } finally {
      setBusy('');
    }
  };
  const cancel = async () => {
    if (
      !acquisition ||
      ['ready', 'failed', 'cancelled'].includes(acquisition.state) ||
      cancelling.current
    )
      return;
    cancelling.current = true;
    cancelledJob.current = acquisition.job;
    setBusy('cancel');
    try {
      await api.extensions.cancelAcquisition(acquisition.job, acquisition.revision);
      setAcquisition(await api.extensions.acquisition(acquisition.job));
    } catch (cause) {
      setError(message(cause));
    } finally {
      cancelling.current = false;
      setBusy('');
    }
  };
  const lifecycle = async (
    extension: ExtensionSummary,
    action: 'enable' | 'disable' | 'retry' | 'remove',
  ) => {
    setBusy(`${action}:${extension.name}`);
    setError('');
    setNotice(null);
    try {
      const result = await api.extensions[`${action}AndWait`](
        extension.name,
        extension.image_digest,
      );
      await reload();
      setNotice(
        result.changed
          ? {
              label: `${extension.name} ${lifecycleResult(action)} and verified.`,
              uncertain: false,
            }
          : {
              label: `${capitalize(action)} was accepted, but the resulting extension state was not observed. Refresh before acting again.`,
              uncertain: true,
            },
      );
    } catch (cause) {
      setError(message(cause));
    } finally {
      setBusy('');
    }
  };
  const requestedContainers = acquisition?.candidate?.requested_containers ?? {
    selectors: [],
    create: false,
  };
  const requestedFilesystem = acquisition?.candidate?.requested_filesystem ?? {
    ...emptyFilesystemGrant(),
  };

  return (
    <Scroll grow height="fill">
      <Column pad={2} gap={2}>
        <Heading label="Extensions" scale="title" />
        <Text
          label="Install, update, enable, disable, and remove workspace extensions."
          color="text-dim"
          wrap
        />
        {!acquisition && <Heading label="Discover" scale="caption" />}
        {!acquisition && (
          <Column gap={2}>
            <Card grow={false} justify="start" width={CONTENT_WIDTH} variant="filled">
              <CardHeader label="Workspace control" detail="First-party · Included" />
              <CardContent gap={1}>
                <Text
                  label="Settings, runtime resources, and terminal panes in one compact tab."
                  color="text-dim"
                  wrap
                />
              </CardContent>
            </Card>
            {catalogue?.entries
              .filter((entry) => !installed.some((extension) => extension.name === entry.id))
              .map((entry) => (
                <Card
                  key={entry.id}
                  grow={false}
                  justify="start"
                  width={CONTENT_WIDTH}
                  variant="filled"
                >
                  <CardHeader label={entry.title} detail={`${entry.publisher} · ${entry.id}`} />
                  <CardContent gap={1}>
                    <Text label={entry.description} color="text-dim" wrap />
                    <Text label={`Source ${entry.source}`} color="text-dim" wrap />
                    <Row>
                      <Button
                        label="Review access"
                        enabled={!busy}
                        onInvoke={() => inspect(entry.reference)}
                      />
                    </Row>
                  </CardContent>
                </Card>
              ))}
            {catalogue && !catalogue.complete && (
              <InlineMessage label="The built-in catalogue is incomplete." tone="warning" />
            )}
            {catalogueError && (
              <InlineMessage label={`Catalogue unavailable: ${catalogueError}`} tone="warning" />
            )}
          </Column>
        )}
        <Card grow={false} justify="start" width={CONTENT_WIDTH} variant="outline">
          <CardHeader label="Install from image" detail="OCI image reference" />
          <CardContent>
            <Row gap={1}>
              <Entry
                value={reference}
                placeholder="registry.example/extension:version"
                onChange={(event: Change) => setReference(String(event.value ?? '').slice(0, 512))}
                onSubmit={() => inspect()}
              />
              <Button
                label={busy === 'inspect' ? 'Inspecting…' : 'Inspect'}
                enabled={Boolean(reference.trim()) && !busy}
                onInvoke={() => inspect()}
              />
            </Row>
          </CardContent>
          {acquisition?.candidate && (
            <CardContent gap={1}>
              <Heading
                label={
                  acquisition.candidate.installed_image_digest ? 'Review update' : 'Review install'
                }
                scale="caption"
              />
              <Text
                label={`Manifest ${acquisition.candidate.name} ${acquisition.candidate.version}`}
              />
              <Text label={`Source ${acquisition.reference}`} color="text-dim" wrap />
              <Text
                label={`Reviewed image ${compactDigest(acquisition.candidate.image_digest)}`}
                tooltip={acquisition.candidate.image_digest}
                wrap
              />
              {acquisition.candidate.installed_image_digest ? (
                <InlineMessage
                  label={`Replaces installed image ${compactDigest(acquisition.candidate.installed_image_digest)}. Access below was reset and must be approved again.`}
                  tone="warning"
                />
              ) : null}
              <Heading label="Review permissions" scale="caption" />
              <InlineMessage
                label="All access is off by default. Enable only what this extension needs."
                tone="warning"
              />
              {acquisition.candidate.requested.length > 0 && (
                <Text label="Husklet access" color="text-dim" />
              )}
              {acquisition.candidate.requested.length > 0 && (
                <Row gap={1} align="center">
                  <Text
                    label={`${granted.length}/${acquisition.candidate.requested.length} allowed`}
                    color="text-dim"
                  />
                  <Button
                    label={
                      granted.length === acquisition.candidate.requested.length
                        ? 'Clear access'
                        : 'Allow requested'
                    }
                    variant="ghost"
                    onInvoke={() =>
                      setGranted(
                        granted.length === acquisition.candidate!.requested.length
                          ? []
                          : acquisition.candidate!.requested,
                      )
                    }
                  />
                </Row>
              )}
              {acquisition.candidate.requested.map((capability) => (
                <FormControlLabel
                  key={capability}
                  label={`${capabilityLabel(capability)} (${capability})`}
                  gap={2}
                >
                  <Switch
                    checked={granted.includes(capability)}
                    onToggle={(event: Change) =>
                      setGranted((current) =>
                        event.value
                          ? [...new Set([...current, capability])]
                          : current.filter((item) => item !== capability),
                      )
                    }
                  />
                </FormControlLabel>
              ))}
              {(requestedContainers.selectors.length > 0 || requestedContainers.create) && (
                <>
                  <Text label="Container access" color="text-dim" />
                  <Text
                    label="Container access starts off. Select only what this extension needs."
                    color="text-dim"
                    wrap
                  />
                </>
              )}
              {requestedContainers.selectors.map((selector) => {
                const key = selectorKey(selector);
                const selected = grantedContainers.selectors.some(
                  (candidate) => selectorKey(candidate) === key,
                );
                return (
                  <FormControlLabel key={key} label={selectorLabel(selector)} gap={2}>
                    <Switch
                      checked={selected}
                      onToggle={(event: Change) =>
                        setGrantedContainers((current) => ({
                          ...current,
                          selectors: event.value
                            ? current.selectors.some((candidate) => selectorKey(candidate) === key)
                              ? current.selectors
                              : [...current.selectors, selector]
                            : current.selectors.filter(
                                (candidate) => selectorKey(candidate) !== key,
                              ),
                        }))
                      }
                    />
                  </FormControlLabel>
                );
              })}
              {requestedContainers.create && (
                <FormControlLabel label="Create new containers" gap={2}>
                  <Switch
                    checked={grantedContainers.create}
                    onToggle={(event: Change) =>
                      setGrantedContainers((current) => ({
                        ...current,
                        create: Boolean(event.value),
                      }))
                    }
                  />
                </FormControlLabel>
              )}
              <Text label="Workspace files" color="text-dim" />
              <FilesystemConsent
                requested={requestedFilesystem}
                granted={grantedFilesystem}
                onChange={setGrantedFilesystem}
              />
              <Button
                label={
                  busy === 'update'
                    ? 'Updating…'
                    : busy === 'install'
                      ? 'Installing…'
                      : acquisition.candidate.installed_image_digest
                        ? 'Update extension'
                        : 'Install extension'
                }
                enabled={!busy && acquisition.state === 'ready'}
                onInvoke={publish}
              />
            </CardContent>
          )}
          {acquisition && acquisition.state !== 'ready' && (
            <CardContent gap={1}>
              <Row gap={1} align="center" wrap>
                {!['failed', 'cancelled'].includes(acquisition.state) && <Spinner />}
                <Text label={acquisitionLabel(acquisition)} wrap />
                {!['failed', 'cancelled'].includes(acquisition.state) ? (
                  <Button
                    label={busy === 'cancel' ? 'Cancelling…' : 'Cancel'}
                    enabled={busy !== 'cancel'}
                    onInvoke={cancel}
                  />
                ) : acquisition.state === 'failed' ? (
                  <>
                    <Button label="Retry inspection" enabled={!busy} onInvoke={() => inspect()} />
                    <Button
                      label="Dismiss"
                      variant="ghost"
                      enabled={!busy}
                      onInvoke={() => setAcquisition(null)}
                    />
                  </>
                ) : (
                  <Button label="Dismiss" enabled={!busy} onInvoke={() => setAcquisition(null)} />
                )}
              </Row>
              {acquisition.error && <InlineMessage label={acquisition.error} tone="danger" />}
            </CardContent>
          )}
        </Card>
        {error && <InlineMessage label={error} tone="danger" />}
        {notice && (
          <InlineMessage label={notice.label} tone={notice.uncertain ? 'warning' : 'positive'} />
        )}
        <Row gap={2}>
          <Heading label="Installed" scale="title" />
          <Button
            label="Refresh"
            enabled={!busy && inventoryState !== 'loading'}
            onInvoke={reload}
          />
        </Row>
        {watchError && <InlineMessage label={watchError} tone="warning" />}
        <ResourceState
          state={inventoryState}
          loadingLabel="Loading installed extensions…"
          emptyLabel="No extensions installed"
          emptyDetail="Install an OCI extension above."
          error={inventoryError || 'Installed extensions could not be loaded.'}
          onRetry={reload}
        >
          {installed.map((extension) => (
            <Card
              key={`${extension.name}:${extension.image_digest}`}
              grow={false}
              justify="start"
              width={CONTENT_WIDTH}
              variant="filled"
            >
              <CardHeader
                label={extension.name}
                detail={extension.version ?? extension.image_digest}
              />
              <CardContent gap={1}>
                <Row gap={1} wrap>
                  <Badge label={extension.enabled ? extension.status : 'disabled'} />
                  <Text
                    label={compactDigest(extension.image_digest)}
                    tooltip={extension.image_digest}
                  />
                </Row>
                <Row gap={1} wrap>
                  {extension.status.startsWith('fault:') ? (
                    <Button
                      label="Retry"
                      enabled={!busy}
                      onInvoke={() => lifecycle(extension, 'retry')}
                    />
                  ) : extension.enabled ? (
                    <Button
                      label="Disable"
                      enabled={!busy}
                      onInvoke={() => lifecycle(extension, 'disable')}
                    />
                  ) : (
                    <Button
                      label="Enable"
                      enabled={!busy}
                      onInvoke={() => lifecycle(extension, 'enable')}
                    />
                  )}
                  <ConfirmAction
                    label="Remove"
                    confirmLabel={`Remove ${extension.name}`}
                    question={`Remove ${extension.name} from this workspace?`}
                    authorityKey={extension.image_digest}
                    enabled={!busy}
                    onConfirm={() => lifecycle(extension, 'remove')}
                  />
                </Row>
              </CardContent>
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

function selectorKey(selector: ContainerSelector): string {
  if ('all' in selector) return 'all';
  if ('id' in selector) return `id:${selector.id}`;
  return `name:${selector.name}`;
}

function selectorLabel(selector: ContainerSelector): string {
  if ('all' in selector) return 'All workspace containers';
  if ('id' in selector) return `Exact container ${selector.id}`;
  return `Container named ${selector.name}`;
}

function acquisitionLabel(acquisition: ExtensionAcquisitionStatus): string {
  const progress = acquisition.progress;
  if (!progress) {
    return acquisition.state === 'failed'
      ? 'Inspection failed.'
      : acquisition.state === 'cancelled'
        ? 'Inspection cancelled.'
        : acquisition.state === 'queued'
          ? 'Waiting to inspect image…'
          : 'Inspecting image…';
  }
  const amount =
    progress.current === null
      ? ''
      : progress.total === null
        ? ` · ${progress.current} bytes`
        : ` · ${progress.current}/${progress.total} bytes (${Math.min(
            100,
            Math.round((progress.current / Math.max(1, progress.total)) * 100),
          )}%)`;
  return `${progress.status}${progress.id ? ` · ${progress.id}` : ''}${amount}`.slice(0, 500);
}

function lifecycleResult(action: 'enable' | 'disable' | 'retry' | 'remove'): string {
  return action === 'enable'
    ? 'enabled'
    : action === 'disable'
      ? 'disabled'
      : action === 'retry'
        ? 'recovered'
        : 'removed';
}

function capitalize(value: string): string {
  return `${value[0].toUpperCase()}${value.slice(1)}`;
}

function capabilityLabel(capability: ExtensionCapability): string {
  const known: Partial<Record<ExtensionCapability, string>> = {
    'workspaces:read': 'View workspace settings',
    'workspaces:control': 'Modify workspace settings',
    'workspaces:events': 'Observe workspace lifecycle',
    'extensions:read': 'View installed extensions',
    'extensions:control': 'Enable, disable, retry, and remove extensions',
    'extensions:install': 'Install and update extensions',
    'containers:read': 'View containers and processes',
    'containers:control': 'Create, start, stop, and remove containers',
    'containers:attach': 'Run commands inside containers',
    'images:read': 'View images',
    'images:write': 'Pull and remove images',
    'volumes:read': 'View volumes',
    'volumes:write': 'Create and remove volumes',
    'networks:read': 'View networks',
    'networks:write': 'Create and modify networks',
    'terminals:read': 'View terminal tabs and panes',
    'terminals:control': 'Create and rearrange terminal panes',
    'terminals:output': 'Read and write terminal text',
    'panes:observe': 'Observe pane interaction',
    'panes:semantic-read': 'Read structured pane interfaces',
    'panes:semantic-control': 'Operate structured pane interfaces',
    'interface:render': 'Render this extension interface',
  };
  if (known[capability]) return known[capability];
  const [resource, authority] = capability.split(':');
  const action =
    authority === 'read'
      ? 'View'
      : authority === 'control'
        ? 'Control'
        : authority === 'install'
          ? 'Install'
          : authority === 'write'
            ? 'Modify'
            : authority === 'output'
              ? 'Read output from'
              : authority === 'observe'
                ? 'Observe'
                : authority === 'render'
                  ? 'Render'
                  : titleWords(authority);
  return `${action} ${titleWords(resource)}`;
}

function titleWords(value = ''): string {
  return value
    .split('-')
    .filter(Boolean)
    .map((word) => `${word[0]?.toUpperCase() ?? ''}${word.slice(1)}`)
    .join(' ');
}

function compactDigest(digest: string): string {
  return digest.length > 32 ? `${digest.slice(0, 19)}…${digest.slice(-8)}` : digest;
}
