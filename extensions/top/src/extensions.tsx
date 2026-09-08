import React from 'react';
import { PROTOCOL } from '@husklet/client';
import {
  Badge,
  Button,
  Card,
  CardContent,
  CardHeader,
  Column,
  ConfirmAction,
  Entry,
  Expander,
  FormControlLabel,
  Heading,
  InlineMessage,
  ResourceState,
  Row,
  Scroll,
  Separator,
  Spinner,
  Switch,
  Text,
  type ExtensionAcquisitionStatus,
  type ExtensionCapability,
  type ExtensionCatalogue,
  type ExtensionCatalogueEntry,
  type ExtensionSummary,
  type ContainerGrant,
  type ContainerSelector,
  type NetworkGrant,
  type NetworkSelector,
  type FilesystemGrant,
  type FilesystemSelector,
  type WorkspaceEnvironmentGrant,
  type WorkspaceApi,
} from '@husklet/react';

type Change = { value?: unknown };
type LifecycleAction = 'enable' | 'disable' | 'retry' | 'remove';
type LifecycleState = { action: LifecycleAction; name: string };

const CONTENT_WIDTH = { minimum: { chars: 48 }, maximum: { chars: 56 } } as const;
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

function filesystemRoots(grant: FilesystemGrant, verb: FilesystemVerb): FilesystemSelector[] {
  return grant[verb] ?? [];
}

function filesystemSelectorKey(selector: FilesystemSelector): string {
  return 'exact' in selector ? `exact:${selector.exact}` : `subtree:${selector.subtree}`;
}

function filesystemConsentLabel(selector: FilesystemSelector, action: string): string {
  return 'exact' in selector
    ? `${action} file · ${selector.exact}`
    : `${action} folder · ${selector.subtree || 'workspace root'}/ and everything inside`;
}

function filesystemGrantCount(grant: FilesystemGrant): number {
  return FILESYSTEM_VERBS.reduce((count, { key }) => count + filesystemRoots(grant, key).length, 0);
}

function catalogueCompatibility(entry: ExtensionCatalogueEntry, architecture: string) {
  if (entry.protocol !== undefined && entry.protocol !== PROTOCOL) {
    return {
      compatible: false,
      label: `Incompatible · requires protocol ${entry.protocol}; this client uses ${PROTOCOL}`,
    } as const;
  }
  if (entry.architectures && architecture && !entry.architectures.includes(architecture)) {
    return {
      compatible: false,
      label: `Incompatible · supports ${entry.architectures.join(', ')}; workspace is ${architecture}`,
    } as const;
  }
  if (entry.protocol === undefined && entry.architectures === undefined) {
    return { compatible: null, label: 'Compatibility not declared' } as const;
  }
  if (entry.architectures && !architecture) {
    return { compatible: null, label: 'Checking workspace architecture compatibility…' } as const;
  }
  return {
    compatible: true,
    label: `Compatible${entry.protocol === undefined ? '' : ` · protocol ${entry.protocol}`}${entry.architectures === undefined ? '' : ` · ${architecture}`}`,
  } as const;
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
      {FILESYSTEM_VERBS.flatMap(({ key, label }) =>
        filesystemRoots(requested, key).map((selector) => (
          <FormControlLabel
            key={`${key}:${filesystemSelectorKey(selector)}`}
            label={filesystemConsentLabel(selector, label)}
            gap={2}
          >
            <Switch
              checked={filesystemRoots(granted, key).some(
                (candidate) => filesystemSelectorKey(candidate) === filesystemSelectorKey(selector),
              )}
              onToggle={(event: Change) =>
                onChange((current) => {
                  const roots = filesystemRoots(current, key);
                  return {
                    ...current,
                    [key]: event.value
                      ? [...roots, selector]
                      : roots.filter(
                          (candidate) =>
                            filesystemSelectorKey(candidate) !== filesystemSelectorKey(selector),
                        ),
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

function InstalledPermissionSummary({ extension }: { extension: ExtensionSummary }) {
  const capabilities = extension.granted ?? [];
  const containerSelectors = extension.containers?.selectors ?? [];
  const containerCount = containerSelectors.length + Number(extension.containers?.create ?? false);
  const networkSelectors = extension.networks?.selectors ?? [];
  const networkCount = networkSelectors.length + Number(extension.networks?.create ?? false);
  const filesystemCount = extension.filesystem ? filesystemGrantCount(extension.filesystem) : 0;
  const environmentRead = extension.workspace_environment?.read ?? [];
  const environmentWrite = extension.workspace_environment?.write ?? [];
  const environmentCount = environmentRead.length + environmentWrite.length;
  const total =
    capabilities.length + containerCount + networkCount + filesystemCount + environmentCount;
  const summary = [
    capabilities.length ? `${capabilities.length} product` : '',
    containerCount ? `${containerCount} container` : '',
    networkCount ? `${networkCount} network` : '',
    filesystemCount ? `${filesystemCount} file` : '',
    environmentCount ? `${environmentCount} environment` : '',
  ]
    .filter(Boolean)
    .join(' · ');
  return (
    <Expander label={`Granted access · ${total ? summary : 'None'}`}>
      <Column gap={1}>
        <Text label="Effective for this installed image digest" color="text-dim" />
        {capabilities.map((capability) => (
          <Text key={capability} label={`${capabilityLabel(capability)} · ${capability}`} wrap />
        ))}
        {containerSelectors.map((selector, index) => (
          <Text
            key={`container:${index}`}
            label={
              'all' in selector
                ? 'Containers · all containers'
                : 'id' in selector
                  ? `Container · exact ID ${selector.id}`
                  : `Container · exact name ${selector.name}`
            }
            wrap
          />
        ))}
        {extension.containers?.create ? <Text label="Containers · create new containers" /> : null}
        {networkSelectors.map((selector, index) => (
          <Text
            key={`network:${index}`}
            label={`Network · ${networkSelectorLabel(selector)}`}
            wrap
          />
        ))}
        {extension.networks?.create ? <Text label="Networks · create new networks" /> : null}
        {extension.filesystem
          ? FILESYSTEM_VERBS.flatMap(({ key, label }) =>
              filesystemRoots(extension.filesystem!, key).map((selector) => (
                <Text
                  key={`${key}:${filesystemSelectorKey(selector)}`}
                  label={filesystemConsentLabel(selector, label)}
                  wrap
                />
              )),
            )
          : null}
        {(['read', 'write'] as const).flatMap((verb) =>
          (extension.workspace_environment?.[verb] ?? []).map((selector, index) => (
            <Text
              key={`${verb}:${index}`}
              label={
                'all' in selector
                  ? `Environment · ${verb} all names`
                  : `Environment · ${verb} ${selector.name} in workspace ${selector.workspace}`
              }
              wrap
            />
          )),
        )}
      </Column>
    </Expander>
  );
}

export function Extensions({ api }: { api: WorkspaceApi }) {
  const [installed, setInstalled] = React.useState<ExtensionSummary[]>([]);
  const [catalogue, setCatalogue] = React.useState<ExtensionCatalogue | null>(null);
  const [catalogueState, setCatalogueState] = React.useState<'loading' | 'ready' | 'error'>(
    'loading',
  );
  const [catalogueError, setCatalogueError] = React.useState('');
  const [workspaceArchitecture, setWorkspaceArchitecture] = React.useState('');
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
  const [grantedNetworks, setGrantedNetworks] = React.useState<NetworkGrant>({
    selectors: [],
    create: false,
  });
  const [grantedFilesystem, setGrantedFilesystem] =
    React.useState<FilesystemGrant>(emptyFilesystemGrant);
  const [grantedWorkspaceEnvironment, setGrantedWorkspaceEnvironment] =
    React.useState<WorkspaceEnvironmentGrant>({ read: [], write: [] });
  const [busy, setBusy] = React.useState('');
  const [error, setError] = React.useState('');
  const [pendingLifecycle, setPendingLifecycle] = React.useState<LifecycleState | null>(null);
  const [lifecycleFailure, setLifecycleFailure] = React.useState<
    (LifecycleState & { detail: string }) | null
  >(null);
  const [notice, setNotice] = React.useState<{ label: string; uncertain: boolean } | null>(null);
  const cancelling = React.useRef(false);
  const cancelledJob = React.useRef('');
  const candidateKey = React.useRef('');
  const inventoryEpoch = React.useRef(0);
  const lifecycleInFlight = React.useRef(false);

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
  const loadCatalogue = React.useCallback(async () => {
    const readCatalogue = api.extensions.catalogue;
    if (!readCatalogue) return;
    setCatalogueState('loading');
    setCatalogueError('');
    try {
      const value = await readCatalogue();
      setCatalogue(value);
      setCatalogueState('ready');
    } catch (cause) {
      setCatalogue(null);
      setCatalogueError(message(cause));
      setCatalogueState('error');
    }
  }, [api]);
  React.useEffect(() => {
    void loadCatalogue();
  }, [loadCatalogue]);
  React.useEffect(() => {
    if (typeof api.info !== 'function') return;
    void api
      .info()
      .then((workspace) => setWorkspaceArchitecture(workspace.architecture))
      .catch(() => setWorkspaceArchitecture(''));
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
            setGrantedNetworks({ selectors: [], create: false });
            setGrantedFilesystem(emptyFilesystemGrant());
            setGrantedWorkspaceEnvironment({ read: [], write: [] });
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
        grantedNetworks,
        grantedFilesystem,
        { workspaceEnvironment: grantedWorkspaceEnvironment },
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
  const dismissReview = () => {
    setAcquisition(null);
    setGranted([]);
    setGrantedContainers({ selectors: [], create: false });
    setGrantedNetworks({ selectors: [], create: false });
    setGrantedFilesystem(emptyFilesystemGrant());
    setGrantedWorkspaceEnvironment({ read: [], write: [] });
    candidateKey.current = '';
    setError('');
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
  const lifecycle = async (extension: ExtensionSummary, action: LifecycleAction) => {
    if (lifecycleInFlight.current) return;
    lifecycleInFlight.current = true;
    const operation = { action, name: extension.name };
    setBusy(`${action}:${extension.name}`);
    setPendingLifecycle(operation);
    setLifecycleFailure(null);
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
      setLifecycleFailure({ ...operation, detail: message(cause) });
    } finally {
      lifecycleInFlight.current = false;
      setPendingLifecycle(null);
      setBusy('');
    }
  };
  const requestedContainers = acquisition?.candidate?.requested_containers ?? {
    selectors: [],
    create: false,
  };
  const requestedNetworks = acquisition?.candidate?.requested_networks ?? {
    selectors: [],
    create: false,
  };
  const requestedFilesystem = acquisition?.candidate?.requested_filesystem ?? {
    ...emptyFilesystemGrant(),
  };
  const availableCatalogue =
    catalogue?.entries.filter(
      (entry) => !installed.some((extension) => extension.name === entry.id),
    ) ?? [];
  const requestedWorkspaceEnvironment = acquisition?.candidate?.requested_workspace_environment ?? {
    read: [],
    write: [],
  };
  const requestedPermissionCount = acquisition?.candidate
    ? acquisition.candidate.requested.length +
      requestedContainers.selectors.length +
      Number(requestedContainers.create) +
      requestedNetworks.selectors.length +
      Number(requestedNetworks.create) +
      filesystemGrantCount(requestedFilesystem) +
      requestedWorkspaceEnvironment.read.length +
      requestedWorkspaceEnvironment.write.length
    : 0;
  const grantedPermissionCount =
    granted.length +
    grantedContainers.selectors.length +
    Number(grantedContainers.create) +
    grantedNetworks.selectors.length +
    Number(grantedNetworks.create) +
    filesystemGrantCount(grantedFilesystem) +
    grantedWorkspaceEnvironment.read.length +
    grantedWorkspaceEnvironment.write.length;

  const content = (
    <Scroll grow height="fill">
      <Column pad={2} gap={2}>
        <Heading label="Extensions" scale="title" />
        <Text
          label="Install, update, enable, disable, and remove workspace extensions."
          color="text-dim"
          wrap
        />
        <Row gap={3} align="start" wrap>
          <Column gap={2} width={CONTENT_WIDTH}>
            {!acquisition && <Heading label="Discover" scale="caption" />}
            {!acquisition && (
              <Column gap={2}>
                {catalogueState === 'loading' && (
                  <Row gap={1} align="center">
                    <Spinner />
                    <Text label="Loading extension catalogue…" color="text-dim" />
                  </Row>
                )}
                {catalogueState === 'ready' && availableCatalogue.length === 0 && (
                  <InlineMessage
                    label="No additional extensions are available in the built-in catalogue."
                    tone="neutral"
                  />
                )}
                {availableCatalogue.map((entry) => {
                  const compatibility = catalogueCompatibility(entry, workspaceArchitecture);
                  return (
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
                        <Text
                          label={`Image · ${entry.reference}`}
                          color="text-dim"
                          tooltip={entry.reference}
                          wrap
                        />
                        <Badge
                          label={compatibility.label}
                          tone={
                            compatibility.compatible === false
                              ? 'danger'
                              : compatibility.compatible === true
                                ? 'positive'
                                : 'neutral'
                          }
                        />
                        <Row>
                          <Button
                            label={`Review ${entry.title}`}
                            enabled={!busy && compatibility.compatible !== false}
                            onInvoke={() => inspect(entry.reference)}
                          />
                        </Row>
                      </CardContent>
                    </Card>
                  );
                })}
                {catalogue && !catalogue.complete && (
                  <InlineMessage label="The built-in catalogue is incomplete." tone="warning" />
                )}
                {catalogueState === 'error' && (
                  <Column gap={1}>
                    <InlineMessage
                      label={`Catalogue unavailable: ${catalogueError}`}
                      tone="warning"
                    />
                    <Row>
                      <Button label="Retry catalogue" onInvoke={loadCatalogue} />
                    </Row>
                  </Column>
                )}
              </Column>
            )}
            <Card grow={false} justify="start" width={CONTENT_WIDTH} variant="outline">
              <CardHeader
                label={
                  acquisition?.candidate
                    ? `Review ${acquisition.candidate.name}`
                    : 'Install from image'
                }
                detail={
                  acquisition?.candidate
                    ? acquisition.candidate.installed_image_digest
                      ? 'Update extension'
                      : 'Install extension'
                    : 'OCI image reference'
                }
              />
              {!acquisition && (
                <CardContent>
                  <Row gap={1}>
                    <Entry
                      value={reference}
                      placeholder="registry.example/extension:version"
                      tooltip={
                        reference || 'Paste a full OCI image reference; press Enter to inspect'
                      }
                      width={{ chars: 40 }}
                      onChange={(event: Change) =>
                        setReference(String(event.value ?? '').slice(0, 512))
                      }
                      onSubmit={() => inspect()}
                    />
                    <Button
                      label={busy === 'inspect' ? 'Inspecting…' : 'Inspect'}
                      enabled={Boolean(reference.trim()) && !busy}
                      onInvoke={() => inspect()}
                    />
                  </Row>
                  <Text
                    label="Paste a full image reference · Enter to inspect. The acquired manifest is authoritative for compatibility and permissions."
                    color="text-dim"
                    wrap
                  />
                </CardContent>
              )}
              {acquisition?.candidate && (
                <CardContent gap={1}>
                  <Text
                    label={`${acquisition.candidate.name} · ${acquisition.candidate.version}`}
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
                  <Text
                    label="Review every permission choice before continuing."
                    color="text-dim"
                  />
                  <Column gap={1}>
                    {acquisition.candidate.requested.length > 0 && (
                      <Text
                        label={`Product access · ${granted.length}/${acquisition.candidate.requested.length}`}
                        color="text-dim"
                      />
                    )}
                    {acquisition.candidate.requested.length > 0 && (
                      <Row gap={1} align="center">
                        {granted.length > 0 && (
                          <Button
                            label="Clear product access"
                            variant="ghost"
                            onInvoke={() => setGranted([])}
                          />
                        )}
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
                        <Text
                          label={`Container access · ${grantedContainers.selectors.length + Number(grantedContainers.create)}/${requestedContainers.selectors.length + Number(requestedContainers.create)}`}
                          color="text-dim"
                        />
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
                                  ? current.selectors.some(
                                      (candidate) => selectorKey(candidate) === key,
                                    )
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
                    {(requestedNetworks.selectors.length > 0 || requestedNetworks.create) && (
                      <>
                        <Text
                          label={`Network access · ${grantedNetworks.selectors.length + Number(grantedNetworks.create)}/${requestedNetworks.selectors.length + Number(requestedNetworks.create)}`}
                          color="text-dim"
                        />
                        <Text
                          label="Network access starts off. Select only the networks this extension needs."
                          color="text-dim"
                          wrap
                        />
                      </>
                    )}
                    {requestedNetworks.selectors.map((selector) => {
                      const key = networkSelectorKey(selector);
                      const selected = grantedNetworks.selectors.some(
                        (candidate) => networkSelectorKey(candidate) === key,
                      );
                      return (
                        <FormControlLabel key={key} label={networkSelectorLabel(selector)} gap={2}>
                          <Switch
                            checked={selected}
                            onToggle={(event: Change) =>
                              setGrantedNetworks((current) => ({
                                ...current,
                                selectors: event.value
                                  ? current.selectors.some(
                                      (candidate) => networkSelectorKey(candidate) === key,
                                    )
                                    ? current.selectors
                                    : [...current.selectors, selector]
                                  : current.selectors.filter(
                                      (candidate) => networkSelectorKey(candidate) !== key,
                                    ),
                              }))
                            }
                          />
                        </FormControlLabel>
                      );
                    })}
                    {requestedNetworks.create && (
                      <FormControlLabel label="Create new networks" gap={2}>
                        <Switch
                          checked={grantedNetworks.create}
                          onToggle={(event: Change) =>
                            setGrantedNetworks((current) => ({
                              ...current,
                              create: Boolean(event.value),
                            }))
                          }
                        />
                      </FormControlLabel>
                    )}
                    {filesystemGrantCount(requestedFilesystem) > 0 && (
                      <>
                        <Text label="Workspace files" color="text-dim" />
                        <FilesystemConsent
                          requested={requestedFilesystem}
                          granted={grantedFilesystem}
                          onChange={setGrantedFilesystem}
                        />
                      </>
                    )}
                    {(requestedWorkspaceEnvironment.read.length > 0 ||
                      requestedWorkspaceEnvironment.write.length > 0) && (
                      <Text
                        label={`Workspace environment values · ${grantedWorkspaceEnvironment.read.length + grantedWorkspaceEnvironment.write.length}/${requestedWorkspaceEnvironment.read.length + requestedWorkspaceEnvironment.write.length}`}
                        color="text-dim"
                      />
                    )}
                    {(['read', 'write'] as const).flatMap((verb) =>
                      requestedWorkspaceEnvironment[verb].map((selector) => {
                        const key = `${verb}:${'all' in selector ? 'all' : `${selector.workspace}:${selector.name}`}`;
                        const checked = grantedWorkspaceEnvironment[verb].some(
                          (candidate) => JSON.stringify(candidate) === JSON.stringify(selector),
                        );
                        return (
                          <FormControlLabel
                            key={key}
                            label={
                              'all' in selector
                                ? `${verb === 'read' ? 'Read' : 'Change'} all workspace environment values`
                                : `${verb === 'read' ? 'Read' : 'Change'} ${selector.name} in workspace ${selector.workspace}`
                            }
                            gap={2}
                          >
                            <Switch
                              checked={checked}
                              onToggle={(event: Change) =>
                                setGrantedWorkspaceEnvironment((current) => ({
                                  ...current,
                                  [verb]: event.value
                                    ? [...current[verb], selector]
                                    : current[verb].filter(
                                        (candidate) =>
                                          JSON.stringify(candidate) !== JSON.stringify(selector),
                                      ),
                                }))
                              }
                            />
                          </FormControlLabel>
                        );
                      }),
                    )}
                  </Column>
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
                        <Button
                          label="Retry inspection"
                          enabled={!busy}
                          onInvoke={() => inspect()}
                        />
                        <Button
                          label="Dismiss"
                          variant="ghost"
                          enabled={!busy}
                          onInvoke={() => setAcquisition(null)}
                        />
                      </>
                    ) : (
                      <Button
                        label="Dismiss"
                        enabled={!busy}
                        onInvoke={() => setAcquisition(null)}
                      />
                    )}
                  </Row>
                  {acquisition.error && (
                    <InlineMessage label={acquisitionFailure(acquisition.error)} tone="danger" />
                  )}
                </CardContent>
              )}
            </Card>
            {error && <InlineMessage label={error} tone="danger" />}
            {notice && (
              <InlineMessage
                label={notice.label}
                tone={notice.uncertain ? 'warning' : 'positive'}
              />
            )}
          </Column>
          <Column gap={2} width={CONTENT_WIDTH}>
            <Row gap={2}>
              <Heading label="Installed" scale="caption" />
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
              emptyDetail="Choose an extension or inspect an OCI image."
              error={inventoryError || 'Installed extensions could not be loaded.'}
              onRetry={reload}
            >
              {installed.map((extension) => {
                const update = catalogue?.entries.find((entry) => entry.id === extension.name);
                const updateCompatibility = update
                  ? catalogueCompatibility(update, workspaceArchitecture)
                  : null;
                return (
                  <Card
                    key={`${extension.name}:${extension.image_digest}`}
                    grow={false}
                    justify="start"
                    width={CONTENT_WIDTH}
                    variant="filled"
                  >
                    <CardHeader
                      label={extension.name}
                      detail={
                        extension.version ? `Version ${extension.version}` : 'Version unavailable'
                      }
                    />
                    <CardContent gap={1}>
                      <Row gap={1} wrap>
                        <Badge
                          label={extensionState(extension)}
                          tone={extension.status.startsWith('fault:') ? 'danger' : 'neutral'}
                        />
                        {extension.name === 'top' ? (
                          <Badge label="Required workspace manager" tone="positive" />
                        ) : null}
                      </Row>
                      <Text
                        label={`Image · ${compactDigest(extension.image_digest)}`}
                        tooltip={extension.image_digest}
                      />
                      <ExtensionFault extension={extension} />
                      <InstalledPermissionSummary extension={extension} />
                      {updateCompatibility ? (
                        <Text
                          label={`Update · ${updateCompatibility.label}`}
                          color={updateCompatibility.compatible === false ? 'warning' : 'text-dim'}
                          wrap
                        />
                      ) : null}
                      <LifecycleFeedback
                        extensionName={extension.name}
                        pending={pendingLifecycle}
                        failure={lifecycleFailure}
                      />
                      <Row gap={1} wrap>
                        {update && (
                          <Button
                            label="Review update"
                            enabled={!busy && updateCompatibility?.compatible !== false}
                            onInvoke={() => inspect(update.reference)}
                          />
                        )}
                        {extension.name === 'top' ? null : extension.status.startsWith('fault:') ? (
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
                        {extension.name !== 'top' && (
                          <ConfirmAction
                            label="Remove"
                            confirmLabel={`Remove ${extension.name}`}
                            question={`Remove ${extension.name} from this workspace?`}
                            authorityKey={extension.image_digest}
                            enabled={!busy}
                            onConfirm={() => lifecycle(extension, 'remove')}
                          />
                        )}
                      </Row>
                    </CardContent>
                  </Card>
                );
              })}
            </ResourceState>
          </Column>
        </Row>
      </Column>
    </Scroll>
  );
  return (
    <Column grow gap={0}>
      {content}
      {acquisition?.candidate ? (
        <Column gap={0}>
          <Separator orientation="horizontal" />
          <Row gap={1} pad={{ top: 1, end: 2, bottom: 1, start: 2 }} wrap>
            <Text
              label={`Review decision · ${grantedPermissionCount}/${requestedPermissionCount} selected`}
              color="text-dim"
            />
            <Button
              label={
                busy === 'update'
                  ? 'Updating…'
                  : busy === 'install'
                    ? 'Installing…'
                    : acquisition.candidate.installed_image_digest
                      ? 'Update with selected access'
                      : 'Install with selected access'
              }
              enabled={!busy && acquisition.state === 'ready'}
              onInvoke={publish}
            />
            <Button
              label="Cancel review"
              variant="ghost"
              enabled={!busy}
              onInvoke={dismissReview}
            />
          </Row>
        </Column>
      ) : null}
    </Column>
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

function networkSelectorKey(selector: NetworkSelector): string {
  if ('all' in selector) return 'all';
  if ('id' in selector) return `id:${selector.id}`;
  return `name:${selector.name}`;
}

function networkSelectorLabel(selector: NetworkSelector): string {
  if ('all' in selector) return 'All workspace networks';
  if ('id' in selector) return `Exact network ${selector.id}`;
  return `Network named ${selector.name}`;
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

function acquisitionFailure(detail: string): string {
  const normalized = detail.replaceAll('\\n', ' ').replaceAll(/\s+/g, ' ').trim();
  const registryMessage = /"message"\s*:\s*"([^"]+)"/.exec(normalized)?.[1];
  if (registryMessage) {
    return `Registry refused the image: ${registryMessage}. Check that the reference exists and is accessible.`.slice(
      0,
      300,
    );
  }
  return normalized.slice(0, 300);
}

function lifecycleResult(action: LifecycleAction): string {
  return action === 'enable'
    ? 'enabled'
    : action === 'disable'
      ? 'disabled'
      : action === 'retry'
        ? 'recovered'
        : 'removed';
}

function lifecyclePending(action: LifecycleAction): string {
  return action === 'enable'
    ? 'Enabling'
    : action === 'disable'
      ? 'Disabling'
      : action === 'retry'
        ? 'Retrying'
        : 'Removing';
}

function extensionState(extension: ExtensionSummary): string {
  if (!extension.enabled) return 'disabled';
  if (extension.status.startsWith('fault:')) return 'faulted';
  return extension.status === 'duty' ? 'enabled' : extension.status;
}

function ExtensionFault({ extension }: { extension: ExtensionSummary }) {
  if (!extension.enabled || !extension.status.startsWith('fault:')) return null;
  const detail =
    extension.status.slice('fault:'.length).trim() || 'The extension stopped unexpectedly.';
  return <InlineMessage label={detail} tone="danger" />;
}

function LifecycleFeedback({
  extensionName,
  pending,
  failure,
}: {
  extensionName: string;
  pending: LifecycleState | null;
  failure: (LifecycleState & { detail: string }) | null;
}) {
  if (pending?.name === extensionName)
    return (
      <InlineMessage
        label={`${lifecyclePending(pending.action)} ${extensionName}…`}
        tone="neutral"
      />
    );
  if (failure?.name === extensionName)
    return (
      <InlineMessage
        label={`${capitalize(failure.action)} failed: ${failure.detail}`}
        tone="danger"
      />
    );
  return null;
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
