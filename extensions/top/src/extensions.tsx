import React from 'react';
import { PROTOCOL } from '@husklet/client';
import {
  Badge,
  Button,
  Card,
  CardActions,
  CardContent,
  CardHeader,
  Column,
  Container,
  ConfirmAction,
  Entry,
  Expander,
  FormControlLabel,
  Heading,
  IconButton,
  InlineMessage,
  Progress,
  RecoveryState,
  Search,
  ResourceState,
  Row,
  Scroll,
  Select,
  Separator,
  Spacer,
  Spinner,
  Switch,
  Text,
  ToggleButton,
  ToggleButtonGroup,
  type ExtensionAcquisitionStatus,
  type ExtensionCapability,
  type ExtensionCatalogue,
  type ExtensionCatalogueEntry,
  type ExtensionPaneProvider,
  type ExtensionSummary,
  type ContainerGrant,
  type ContainerSelector,
  type ImageGrant,
  type ImageSelector,
  type NetworkGrant,
  type NetworkSelector,
  type VolumeGrant,
  type VolumeSelector,
  type FilesystemGrant,
  type FilesystemSelector,
  type WorkspaceEnvironmentGrant,
  type WorkspaceApi,
} from '@husklet/react';

type Change = { value?: unknown };
type ExtensionMode = 'installed' | 'discover';
type LifecycleAction = 'enable' | 'disable' | 'retry' | 'remove';
type LifecycleState = { action: LifecycleAction; name: string };
type ProviderFailure = { key: string; detail: string; retry: boolean };
export type CatalogueFilter =
  'discover' | 'all' | 'available' | 'installed' | 'updates' | 'incompatible';
export type InstalledFilter = 'all' | 'running' | 'faulted' | 'updates' | 'disabled';
type ImageVerb = 'read' | 'use' | 'pull' | 'remove';
const IMAGE_VERBS: { key: ImageVerb; label: string }[] = [
  { key: 'read', label: 'View image' },
  { key: 'use', label: 'Use image for new containers' },
  { key: 'pull', label: 'Pull image' },
  { key: 'remove', label: 'Remove image' },
];

function imageCapability(verb: ImageVerb): ExtensionCapability {
  switch (verb) {
    case 'read':
      return 'images:read';
    case 'use':
      return 'containers:create';
    case 'pull':
      return 'images:pull';
    case 'remove':
      return 'images:remove';
  }
}

const COPY_WIDTH = { maximum: { chars: 54 } } as const;
const PAGE_WIDTH = { maximum: { chars: 110 } } as const;
const CATALOGUE_PAGE_SIZE = 8;
const INSTALLED_PAGE_SIZE = 12;
const FILESYSTEM_VERBS = [
  { key: 'read', label: 'View contents', meaning: 'read' },
  { key: 'write', label: 'Modify existing contents', meaning: 'write' },
  { key: 'create', label: 'Create new entries', meaning: 'create' },
  { key: 'delete', label: 'Delete entries', meaning: 'delete' },
  { key: 'rename', label: 'Rename or move entries', meaning: 'rename' },
] as const;
type FilesystemVerb = (typeof FILESYSTEM_VERBS)[number]['key'];

function withCapability(
  current: ExtensionCapability[],
  capability: ExtensionCapability,
  enabled: boolean,
) {
  return enabled
    ? [...new Set([...current, capability])]
    : current.filter((item) => item !== capability);
}

function emptyFilesystemGrant(): Required<FilesystemGrant> {
  return { read: [], write: [], create: [], delete: [], rename: [] };
}

function isInstalledCandidateUnchanged(
  candidate: ExtensionAcquisitionStatus['candidate'],
): boolean {
  return Boolean(
    candidate?.installed_image_digest &&
    candidate.installed_image_digest === candidate.image_digest,
  );
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

export function catalogueTrust(entry: ExtensionCatalogueEntry) {
  return entry.publisher_verified
    ? { label: `Verified publisher · ${entry.publisher}`, tone: 'accent' as const }
    : { label: `Publisher · ${entry.publisher}`, tone: 'neutral' as const };
}

export function catalogueCandidateMismatch(
  entry: Pick<ExtensionCatalogueEntry, 'id' | 'version' | 'reference'> | null,
  candidate: { name: string; version: string } | null | undefined,
  acquiredReference?: string,
) {
  if (!entry || !candidate) return '';
  if (acquiredReference !== undefined && acquiredReference !== entry.reference)
    return `Catalogue image changed: expected the selected image reference, but the acquisition completed for a different reference.`;
  if (candidate.name !== entry.id)
    return `Catalogue identity changed: expected ${entry.id}, but the inspected image declares ${candidate.name}.`;
  if (candidate.version !== entry.version)
    return `Catalogue version changed: expected ${entry.version}, but the inspected image declares ${candidate.version}.`;
  return '';
}

export function compactImageReference(reference: string) {
  const separator = reference.lastIndexOf('@');
  if (separator < 0) return reference;
  const name = reference.slice(0, separator);
  const digest = reference.slice(separator + 1);
  return `${name} · ${compactDigest(digest)}`;
}

function compareCatalogueEntries(left: ExtensionCatalogueEntry, right: ExtensionCatalogueEntry) {
  const leftKey = `${left.title.toLowerCase()}\0${left.id.toLowerCase()}`;
  const rightKey = `${right.title.toLowerCase()}\0${right.id.toLowerCase()}`;
  return leftKey < rightKey ? -1 : leftKey > rightKey ? 1 : 0;
}

function catalogueEntryMatches(
  entry: ExtensionCatalogueEntry,
  installed: ExtensionSummary[],
  architecture: string,
  query: string,
  filter: CatalogueFilter,
  category: string,
) {
  const installedExtension = installed.find((extension) => extension.name === entry.id);
  const updateAvailable = Boolean(
    installedExtension && newerVersion(entry.version, installedExtension.version),
  );
  const incompatible = catalogueCompatibility(entry, architecture).compatible === false;
  const statusMatches =
    (filter === 'discover' && (!installedExtension || updateAvailable)) ||
    filter === 'all' ||
    (filter === 'available' && !installedExtension) ||
    (filter === 'installed' && Boolean(installedExtension)) ||
    (filter === 'updates' && updateAvailable) ||
    (filter === 'incompatible' && incompatible);
  if (!statusMatches) return false;
  if (category && !(entry.categories ?? []).includes(category)) return false;
  const needle = query.trim().toLocaleLowerCase();
  if (!needle) return true;
  return [
    entry.title,
    entry.id,
    entry.publisher,
    entry.description,
    entry.version,
    entry.source,
    entry.reference,
    ...(entry.categories ?? []),
    ...(entry.architectures ?? []),
    entry.publisher_verified ? 'verified publisher' : 'community publisher',
  ].some((value) => value.toLocaleLowerCase().includes(needle));
}

export function filterCatalogueEntries(
  entries: ExtensionCatalogueEntry[],
  installed: ExtensionSummary[],
  architecture: string,
  query: string,
  filter: CatalogueFilter,
  category = '',
) {
  return entries
    .filter((entry) =>
      catalogueEntryMatches(entry, installed, architecture, query, filter, category),
    )
    .sort(compareCatalogueEntries);
}

function installedUpdate(
  extension: ExtensionSummary,
  catalogue: ExtensionCatalogueEntry[],
): ExtensionCatalogueEntry | undefined {
  return catalogue.find(
    (entry) => entry.id === extension.name && newerVersion(entry.version, extension.version),
  );
}

function installedPriority(
  extension: ExtensionSummary,
  catalogue: ExtensionCatalogueEntry[],
): number {
  if (extension.status.startsWith('fault:')) return 0;
  if (installedUpdate(extension, catalogue)) return 1;
  if (!extension.enabled || extension.status === 'standby') return 2;
  if (extension.enabled) return 3;
  return 4;
}

export function filterInstalledExtensions(
  extensions: ExtensionSummary[],
  catalogue: ExtensionCatalogueEntry[],
  query: string,
  filter: InstalledFilter,
) {
  const needle = query.trim().toLocaleLowerCase();
  return extensions
    .filter((extension) => {
      const faulted = extension.status.startsWith('fault:');
      const update = Boolean(installedUpdate(extension, catalogue));
      const disabled = !extension.enabled || extension.status === 'standby';
      const statusMatches =
        filter === 'all' ||
        (filter === 'running' && extension.enabled && !faulted) ||
        (filter === 'faulted' && faulted) ||
        (filter === 'updates' && update) ||
        (filter === 'disabled' && disabled);
      if (!statusMatches) return false;
      if (!needle) return true;
      return [
        extension.name,
        extension.status,
        ...(extension.pane_providers ?? []).flatMap((provider) => [provider.id, provider.title]),
      ].some((value) => value.toLocaleLowerCase().includes(needle));
    })
    .sort((left, right) => {
      const priority = installedPriority(left, catalogue) - installedPriority(right, catalogue);
      if (priority !== 0) return priority;
      const leftName = left.name.toLocaleLowerCase();
      const rightName = right.name.toLocaleLowerCase();
      return leftName < rightName ? -1 : leftName > rightName ? 1 : 0;
    });
}

function FilesystemConsent({
  requested,
  granted,
  onChange,
  onCapabilityChange,
}: {
  requested: FilesystemGrant;
  granted: FilesystemGrant;
  onChange: React.Dispatch<React.SetStateAction<FilesystemGrant>>;
  onCapabilityChange: (capability: ExtensionCapability, enabled: boolean) => void;
}) {
  const requestCount = FILESYSTEM_VERBS.reduce(
    (count, { key }) => count + filesystemRoots(requested, key).length,
    0,
  );
  if (requestCount === 0) return <Text label="No workspace paths requested." color="text-dim" />;

  return (
    <Column gap={1}>
      <Text
        label="Each switch grants only the named action and root, and includes the matching file capability. Modify cannot create, delete, or rename."
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
              onToggle={(event: Change) => {
                const roots = filesystemRoots(granted, key);
                const next = {
                  ...granted,
                  [key]: event.value
                    ? [...roots, selector]
                    : roots.filter(
                        (candidate) =>
                          filesystemSelectorKey(candidate) !== filesystemSelectorKey(selector),
                      ),
                };
                const capability = key === 'read' ? 'filesystem:read' : 'filesystem:write';
                const enabled =
                  capability === 'filesystem:read'
                    ? filesystemRoots(next, 'read').length > 0
                    : (['write', 'create', 'delete', 'rename'] as const).some(
                        (verb) => filesystemRoots(next, verb).length > 0,
                      );
                onCapabilityChange(capability, enabled);
                onChange(next);
              }}
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
  const volumeSelectors = extension.volumes?.selectors ?? [];
  const volumeCount = volumeSelectors.length + Number(extension.volumes?.create ?? false);
  const filesystemCount = extension.filesystem ? filesystemGrantCount(extension.filesystem) : 0;
  const environmentRead = extension.workspace_environment?.read ?? [];
  const environmentWrite = extension.workspace_environment?.write ?? [];
  const environmentCount = environmentRead.length + environmentWrite.length;
  const total =
    capabilities.length +
    containerCount +
    networkCount +
    volumeCount +
    filesystemCount +
    environmentCount;
  const summary = [
    capabilities.length === 1
      ? capabilityLabel(capabilities[0])
      : capabilities.length
        ? countLabel(capabilities.length, 'API scope')
        : '',
    containerCount ? countLabel(containerCount, 'container rule') : '',
    networkCount ? countLabel(networkCount, 'network rule') : '',
    volumeCount ? countLabel(volumeCount, 'volume rule') : '',
    filesystemCount ? countLabel(filesystemCount, 'file rule') : '',
    environmentCount ? countLabel(environmentCount, 'environment rule') : '',
  ]
    .filter(Boolean)
    .join(' · ');
  return (
    <Expander label={`Granted access · ${total ? summary : 'None'}`}>
      <Column gap={1}>
        <Text label="Effective for this installed image digest" color="text-dim" />
        {capabilities.map((capability) => (
          <Text key={capability} label={capabilityLabel(capability)} wrap />
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
        {volumeSelectors.map((selector, index) => (
          <Text key={`volume:${index}`} label={`Volume · ${volumeSelectorLabel(selector)}`} wrap />
        ))}
        {extension.volumes?.create ? <Text label="Volumes · create new volumes" /> : null}
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

function countLabel(count: number, singular: string): string {
  return `${count} ${singular}${count === 1 ? '' : 's'}`;
}

function RequestedPermissionSummary({ groups }: { groups: { label: string; count: number }[] }) {
  const requested = groups.filter(({ count }) => count > 0);
  if (requested.length === 0) {
    return <Text label="No workspace access requested" color="text-dim" />;
  }
  return (
    <Column gap={0}>
      <Text label="Requested access" color="text-dim" />
      <Text
        label={requested.map(({ label, count }) => `${label} · ${count}`).join('  ·  ')}
        color="text-dim"
        wrap
      />
    </Column>
  );
}

export function Extensions({ api }: { api: WorkspaceApi }) {
  const [mode, setMode] = React.useState<ExtensionMode>('installed');
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
  const [catalogueExpectation, setCatalogueExpectation] =
    React.useState<ExtensionCatalogueEntry | null>(null);
  const [granted, setGranted] = React.useState<ExtensionCapability[]>([]);
  const [grantedContainers, setGrantedContainers] = React.useState<ContainerGrant>({
    selectors: [],
    create: false,
  });
  const [grantedImages, setGrantedImages] = React.useState<ImageGrant>({
    read: [],
    use: [],
    pull: [],
    remove: [],
    prune_all_unused: false,
  });
  const [grantedNetworks, setGrantedNetworks] = React.useState<NetworkGrant>({
    selectors: [],
    create: false,
  });
  const [grantedVolumes, setGrantedVolumes] = React.useState<VolumeGrant>({
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
  const [opening, setOpening] = React.useState('');
  const [providerFailure, setProviderFailure] = React.useState<ProviderFailure | null>(null);
  const [catalogueQuery, setCatalogueQuery] = React.useState('');
  const [catalogueFilter, setCatalogueFilter] = React.useState<CatalogueFilter>('discover');
  const [catalogueCategory, setCatalogueCategory] = React.useState('');
  const [catalogueLimit, setCatalogueLimit] = React.useState(CATALOGUE_PAGE_SIZE);
  const [installedQuery, setInstalledQuery] = React.useState('');
  const [installedFilter, setInstalledFilter] = React.useState<InstalledFilter>('all');
  const [installedLimit, setInstalledLimit] = React.useState(INSTALLED_PAGE_SIZE);
  const [permissionDetailsExpanded, setPermissionDetailsExpanded] = React.useState(false);
  const cancelling = React.useRef(false);
  const cancelledJob = React.useRef('');
  const candidateKey = React.useRef('');
  const inventoryEpoch = React.useRef(0);
  const catalogueEpoch = React.useRef(0);
  const lifecycleInFlight = React.useRef(false);
  const acquisitionInFlight = React.useRef(false);
  const openingInFlight = React.useRef('');

  const selectMode = (next: ExtensionMode) => {
    if (next === mode) return;
    setMode(next);
    setCatalogueQuery('');
    setCatalogueFilter('discover');
    setCatalogueCategory('');
    setCatalogueLimit(CATALOGUE_PAGE_SIZE);
    setInstalledQuery('');
    setInstalledFilter('all');
    setInstalledLimit(INSTALLED_PAGE_SIZE);
  };

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
    const epoch = ++catalogueEpoch.current;
    setCatalogueState('loading');
    setCatalogueError('');
    try {
      const value = await readCatalogue();
      if (catalogueEpoch.current !== epoch) return;
      setCatalogue(value);
      setCatalogueState('ready');
    } catch (cause) {
      if (catalogueEpoch.current !== epoch) return;
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

  const inspect = async (suggested?: string, expected: ExtensionCatalogueEntry | null = null) => {
    const wanted = (suggested ?? reference).trim();
    if (!wanted || busy || acquisitionInFlight.current) return;
    acquisitionInFlight.current = true;
    setReference(wanted);
    setCatalogueExpectation(expected);
    setBusy('inspect');
    setError('');
    setNotice(null);
    setPermissionDetailsExpanded(false);
    setAcquisition(null);
    candidateKey.current = '';
    try {
      const started = await api.extensions.startAcquisition(wanted);
      cancelledJob.current = '';
      let status = await api.extensions.acquisition(started.job);
      while (true) {
        if (!isInstalledCandidateUnchanged(status.candidate)) setAcquisition(status);
        if (status.candidate && !isInstalledCandidateUnchanged(status.candidate)) {
          const key = `${status.job}:${status.candidate.image_digest}`;
          if (candidateKey.current !== key) {
            candidateKey.current = key;
            // Every authority is opt-in. Inspection must never grant access,
            // including during an update where a manifest may have widened.
            setGranted([]);
            setGrantedContainers({ selectors: [], create: false });
            setGrantedImages({ read: [], use: [], pull: [], remove: [], prune_all_unused: false });
            setGrantedNetworks({ selectors: [], create: false });
            setGrantedVolumes({ selectors: [], create: false });
            setGrantedFilesystem(emptyFilesystemGrant());
            setGrantedWorkspaceEnvironment({ read: [], write: [] });
            setPermissionDetailsExpanded((status.candidate.required?.length ?? 0) > 0);
          }
        }
        if (
          status.state === 'ready' ||
          status.state === 'failed' ||
          status.state === 'cancelled' ||
          cancelledJob.current === started.job
        )
          break;
        const changed = await api.extensions.waitForAcquisition(started.job, status.revision, {
          // Each socket wait stays bounded, while the review remains attached
          // to the host-owned job for however long the registry operation needs.
          timeoutMs: 1_000,
        });
        if (changed.changed) status = changed.status;
      }
      if (isInstalledCandidateUnchanged(status.candidate)) {
        setAcquisition(null);
        setNotice({
          label: `${status.candidate?.name ?? wanted} is up to date. The reviewed image already matches the installed image; access was not changed.`,
          uncertain: false,
        });
      }
    } catch (cause) {
      setError(message(cause));
    } finally {
      acquisitionInFlight.current = false;
      setBusy('');
    }
  };
  const publish = async () => {
    if (
      !acquisition?.candidate ||
      acquisition.state !== 'ready' ||
      isInstalledCandidateUnchanged(acquisition.candidate) ||
      catalogueCandidateMismatch(
        catalogueExpectation,
        acquisition.candidate,
        acquisition.reference,
      ) ||
      busy
    )
      return;
    const updating = Boolean(acquisition.candidate.installed_image_digest);
    const reviewed = acquisition.candidate;
    setBusy(updating ? 'update' : 'install');
    setError('');
    setNotice(null);
    try {
      const result = await api.extensions[updating ? 'updateAndWait' : 'installAndWait'](
        acquisition.job,
        acquisition.revision,
        {
          capabilities: granted,
          containers: grantedContainers,
          images: grantedImages,
          networks: grantedNetworks,
          volumes: grantedVolumes,
          filesystem: grantedFilesystem,
          workspaceEnvironment: grantedWorkspaceEnvironment,
        },
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
      try {
        let status = await api.extensions.acquisition(acquisition.job);
        if (status.state === 'committing') {
          setAcquisition(status);
          setError('');
          // A vanished reply does not mean the commit stopped. Follow the
          // authoritative job instead of leaving a stale, replayable consent
          // form on screen while the host may be publishing it.
          const deadline = Date.now() + 30_000;
          while (status.state === 'committing') {
            const remaining = deadline - Date.now();
            if (remaining <= 0) break;
            const changed = await api.extensions.waitForAcquisition(status.job, status.revision, {
              timeoutMs: Math.min(1_000, remaining),
            });
            if (!changed.changed) continue;
            status = changed.status;
            setAcquisition(status);
          }
        }
        if (status.state === 'failed' || status.state === 'cancelled') {
          setAcquisition(status);
          setError('');
        } else if (status.state === 'installed' || status.state === 'updated') {
          const listing = await api.extensions.list();
          const committed = listing.find(
            (extension) =>
              extension.name === reviewed.name && extension.image_digest === reviewed.image_digest,
          );
          if (committed) {
            setInstalled(listing);
            setAcquisition(null);
            setReference('');
            setNotice({
              label: `${reviewed.name} ${updating ? 'updated' : 'installed'}, but the confirmation reply was lost. Current extension state was verified by refresh.`,
              uncertain: false,
            });
          } else {
            setError(message(cause));
          }
        } else if (status.state === 'committing') {
          setError(
            'The install is still being saved after its confirmation reply was lost. Wait for completion before acting again.',
          );
        } else if (status.state === 'ready' && status.candidate) {
          setAcquisition(status);
          setError(
            `The extension was not saved. Its reviewed image and selected access are retained for a safe retry. ${message(cause)}`,
          );
        } else {
          setAcquisition(status);
          setError(message(cause));
        }
      } catch {
        setError(message(cause));
      }
    } finally {
      setBusy('');
    }
  };
  const dismissReview = () => {
    setAcquisition(null);
    setCatalogueExpectation(null);
    setGranted([]);
    setGrantedContainers({ selectors: [], create: false });
    setGrantedImages({ read: [], use: [], pull: [], remove: [], prune_all_unused: false });
    setGrantedNetworks({ selectors: [], create: false });
    setGrantedVolumes({ selectors: [], create: false });
    setGrantedFilesystem(emptyFilesystemGrant());
    setGrantedWorkspaceEnvironment({ read: [], write: [] });
    candidateKey.current = '';
    setError('');
  };
  const cancel = async () => {
    if (
      !acquisition ||
      ['ready', 'committing', 'installed', 'updated', 'failed', 'cancelled'].includes(
        acquisition.state,
      ) ||
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
      cancelledJob.current = '';
      try {
        setAcquisition(await api.extensions.acquisition(acquisition.job));
        setError(
          'Acquisition advanced before cancellation. Review its current phase and cancel again if needed.',
        );
      } catch {
        setError(
          `Cancellation failed: ${message(cause)} Retry inspection to reconcile its current state.`,
        );
      }
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
      try {
        const listing = await api.extensions.list();
        const current = listing.find((item) => item.name === extension.name);
        if (lifecycleStateObserved(action, extension, current)) {
          setInstalled(listing);
          setLifecycleFailure(null);
          setNotice({
            label: `${extension.name} ${lifecycleResult(action)}, but the confirmation reply was lost. Current extension state was verified by refresh.`,
            uncertain: false,
          });
          return;
        }
      } catch {
        // Preserve the original operation failure when reconciliation is also unavailable.
      }
      setLifecycleFailure({ ...operation, detail: message(cause) });
    } finally {
      lifecycleInFlight.current = false;
      setPendingLifecycle(null);
      setBusy('');
    }
  };
  const openProvider = async (extension: ExtensionSummary, provider: ExtensionPaneProvider) => {
    const operation = `open:${extension.name}:${provider.id}`;
    if (busy || openingInFlight.current === operation) return;
    openingInFlight.current = operation;
    setOpening(operation);
    setProviderFailure((failure) => (failure?.key === operation ? null : failure));
    setError('');
    setNotice(null);
    let openedTab = '';
    let mounted = false;
    try {
      const opened = await api.terminal.openTabAndWait(provider.title);
      openedTab = opened.tab;
      if (!opened.changed) {
        throw new Error('the new tab did not publish an observable pane');
      }
      const switched = await api.terminal.switchOccupantAndWait(
        opened.pane.slot,
        opened.pane.generation,
        opened.pane.revision,
        { kind: 'surface', extension: extension.name, provider: provider.id },
      );
      if (!switched.changed) {
        throw new Error('the extension surface did not become the pane occupant');
      }
      mounted = true;
      await api.terminal.focus(switched.pane.slot);
      setNotice({
        label: `${provider.title} opened in a new tab.`,
        uncertain: false,
      });
    } catch (cause) {
      setProviderFailure({
        key: operation,
        retry: Boolean(openedTab),
        detail: mounted
          ? `${provider.title} opened in tab ${openedTab}, but it could not be focused: ${message(cause)}`
          : openedTab
            ? `Tab ${openedTab} was created, but ${provider.title} did not open: ${message(cause)}`
            : `${provider.title} could not be opened: ${message(cause)}`,
      });
    } finally {
      if (openingInFlight.current === operation) openingInFlight.current = '';
      setOpening((current) => (current === operation ? '' : current));
    }
  };
  const providerAction = (extension: ExtensionSummary, provider: ExtensionPaneProvider) => {
    const key = `open:${extension.name}:${provider.id}`;
    const active = opening === key;
    const failure = providerFailure?.key === key ? providerFailure : null;
    return (
      <Column gap={1}>
        <Row gap={1} align="center" wrap>
          {active ? <Spinner /> : null}
          <Button
            label={active ? 'Opening…' : failure?.retry ? 'Retry opening' : 'Open'}
            tooltip={`Open ${provider.title}`}
            size="small"
            variant="filled"
            tone="accent"
            enabled={!busy && !active}
            onInvoke={() => openProvider(extension, provider)}
          />
        </Row>
        {failure ? <InlineMessage label={failure.detail} tone="danger" /> : null}
      </Column>
    );
  };
  const requestedContainers = acquisition?.candidate?.requested_containers ?? {
    selectors: [],
    create: false,
  };
  const requestedImages = acquisition?.candidate?.requested_images ?? {
    read: [],
    use: [],
    pull: [],
    remove: [],
    prune_all_unused: false,
  };
  const requestedNetworks = acquisition?.candidate?.requested_networks ?? {
    selectors: [],
    create: false,
  };
  const requestedVolumes = acquisition?.candidate?.requested_volumes ?? {
    selectors: [],
    create: false,
  };
  const requestedFilesystem = acquisition?.candidate?.requested_filesystem ?? {
    ...emptyFilesystemGrant(),
  };
  const catalogueEntries = React.useMemo(() => catalogue?.entries ?? [], [catalogue]);
  const catalogueCategories = React.useMemo(
    () => [...new Set(catalogueEntries.flatMap((entry) => entry.categories ?? []))].sort(),
    [catalogueEntries],
  );
  const visibleCatalogueEntries = React.useMemo(
    () =>
      filterCatalogueEntries(
        catalogueEntries,
        installed,
        workspaceArchitecture,
        catalogueQuery,
        catalogueFilter,
        catalogueCategory,
      ),
    [
      catalogueCategory,
      catalogueEntries,
      catalogueFilter,
      catalogueQuery,
      installed,
      workspaceArchitecture,
    ],
  );
  const renderedCatalogueEntries = visibleCatalogueEntries.slice(0, catalogueLimit);
  const visibleInstalled = React.useMemo(
    () => filterInstalledExtensions(installed, catalogueEntries, installedQuery, installedFilter),
    [catalogueEntries, installed, installedFilter, installedQuery],
  );
  const renderedInstalled = visibleInstalled.slice(0, installedLimit);
  const requestedWorkspaceEnvironment = acquisition?.candidate?.requested_workspace_environment ?? {
    read: [],
    write: [],
  };
  const requiredCapabilities = acquisition?.candidate?.required ?? [];
  const missingRequiredCapabilities = requiredCapabilities.filter(
    (capability) => !granted.includes(capability),
  );
  const catalogueMismatch = catalogueCandidateMismatch(
    catalogueExpectation,
    acquisition?.candidate,
    acquisition?.reference,
  );
  const requestedPermissionCount = acquisition?.candidate
    ? acquisition.candidate.requested.length +
      requestedContainers.selectors.length +
      Number(requestedContainers.create) +
      imageGrantCount(requestedImages) +
      requestedNetworks.selectors.length +
      Number(requestedNetworks.create) +
      requestedVolumes.selectors.length +
      Number(requestedVolumes.create) +
      filesystemGrantCount(requestedFilesystem) +
      requestedWorkspaceEnvironment.read.length +
      requestedWorkspaceEnvironment.write.length
    : 0;
  const grantedPermissionCount =
    granted.length +
    grantedContainers.selectors.length +
    Number(grantedContainers.create) +
    imageGrantCount(grantedImages) +
    grantedNetworks.selectors.length +
    Number(grantedNetworks.create) +
    grantedVolumes.selectors.length +
    Number(grantedVolumes.create) +
    filesystemGrantCount(grantedFilesystem) +
    grantedWorkspaceEnvironment.read.length +
    grantedWorkspaceEnvironment.write.length;
  const requestedPermissionGroups = acquisition?.candidate
    ? [
        { label: 'Product', count: acquisition.candidate.requested.length },
        {
          label: 'Containers',
          count: requestedContainers.selectors.length + Number(requestedContainers.create),
        },
        { label: 'Images', count: imageGrantCount(requestedImages) },
        {
          label: 'Networks',
          count: requestedNetworks.selectors.length + Number(requestedNetworks.create),
        },
        {
          label: 'Volumes',
          count: requestedVolumes.selectors.length + Number(requestedVolumes.create),
        },
        { label: 'Files', count: filesystemGrantCount(requestedFilesystem) },
        {
          label: 'Environment',
          count:
            requestedWorkspaceEnvironment.read.length + requestedWorkspaceEnvironment.write.length,
        },
      ]
    : [];
  const identityReviewWarning = acquisition?.candidate
    ? [
        catalogueExpectation
          ? !catalogueExpectation.publisher_verified
            ? 'Publisher is not verified; confirm the catalogue source and reviewed digest.'
            : null
          : 'Direct OCI image has no catalogue publisher verification; confirm its source and digest.',
        acquisition.candidate.installed_image_digest
          ? `Image changes from ${compactDigest(acquisition.candidate.installed_image_digest)}; access has been reset.`
          : null,
      ]
        .filter(Boolean)
        .join(' ')
    : '';
  const privilegedAccessWarning = [
    requestedImages.remove.length > 0 || requestedImages.prune_all_unused
      ? 'Image removal can delete named images or every unused workspace image.'
      : null,
    acquisition?.candidate?.requested.includes('workspaces:control')
      ? 'Workspace lifecycle access can create or delete workspaces and start or stop workloads.'
      : null,
  ]
    .filter(Boolean)
    .join(' ');

  const content = (
    <Scroll grow width="fill" height="fill">
      <Container pad={4} gap={3} width={PAGE_WIDTH}>
        <Heading label="Extensions" scale="display" />
        <Text
          label="Discover tools, review their access, and manage what runs in this workspace."
          color="text-dim"
          wrap
        />
        <ToggleButtonGroup gap={0} width="content">
          <ToggleButton
            label="Installed"
            selected={mode === 'installed'}
            onToggle={() => selectMode('installed')}
          />
          <ToggleButton
            label="Discover"
            selected={mode === 'discover'}
            onToggle={() => selectMode('discover')}
          />
        </ToggleButtonGroup>
        {error && <RecoveryState operation="Extension change" error={error} />}
        {notice && (
          <InlineMessage label={notice.label} tone={notice.uncertain ? 'warning' : 'positive'} />
        )}
        <Column gap={3} width="fill">
          {mode === 'discover' || acquisition ? (
            <Column gap={2} width="fill">
              {!acquisition && (
                <Row gap={1} width="fill" align="center" justify="start" wrap>
                  <Heading label="Discover" scale="caption" grow={false} align="start" />
                  {catalogueState === 'ready' && catalogueEntries.length > 0 ? (
                    <Badge
                      label={`${countLabel(catalogueEntries.length, 'extension')}${
                        catalogueEntries.every((entry) =>
                          installed.some((extension) => extension.name === entry.id),
                        )
                          ? ' · all installed'
                          : ''
                      }`}
                    />
                  ) : null}
                </Row>
              )}
              {!acquisition && (
                <Column gap={2}>
                  <Text
                    label="Browse available tools. Husklet inspects the image first; nothing is installed until you approve its exact access."
                    color="text-dim"
                    width={COPY_WIDTH}
                    wrap
                  />
                  {catalogueState === 'ready' && catalogueEntries.length > 0 ? (
                    <Column gap={1} width="fill">
                      <Text label="Find extensions" color="text-dim" />
                      <Row gap={1} width="fill" wrap align="center" justify="start">
                        <Search
                          grow
                          value={catalogueQuery}
                          placeholder="Search extensions"
                          tooltip="Search by name, identifier, publisher, or description"
                          width={{ minimum: { chars: 18 }, maximum: { chars: 36 } }}
                          onChange={(event: Change) => {
                            setCatalogueQuery(String(event.value ?? '').slice(0, 128));
                            setCatalogueLimit(CATALOGUE_PAGE_SIZE);
                          }}
                        />
                        <Select
                          value={catalogueFilter}
                          tooltip="Filter extension catalogue by status"
                          width={{ minimum: { chars: 22 }, maximum: { chars: 22 } }}
                          choices={[
                            { value: 'discover', label: 'Available & updates' },
                            { value: 'all', label: 'All extensions' },
                            { value: 'available', label: 'Available' },
                            { value: 'installed', label: 'Installed' },
                            { value: 'updates', label: 'Updates' },
                            { value: 'incompatible', label: 'Incompatible' },
                          ]}
                          onChange={(event: Change) => {
                            const selected = String(event.value ?? '');
                            if (
                              [
                                'discover',
                                'all',
                                'available',
                                'installed',
                                'updates',
                                'incompatible',
                              ].includes(selected)
                            ) {
                              setCatalogueFilter(selected as CatalogueFilter);
                              setCatalogueLimit(CATALOGUE_PAGE_SIZE);
                            }
                          }}
                        />
                        <Text
                          label={`${visibleCatalogueEntries.length} of ${countLabel(catalogueEntries.length, 'extension')}`}
                          color="text-dim"
                        />
                      </Row>
                      <Row gap={1} width="fill" align="center" justify="start">
                        <Text label="Category" color="text-dim" />
                        <Select
                          value={catalogueCategory}
                          tooltip="Filter extension catalogue by category"
                          width={{ minimum: { chars: 18 }, maximum: { chars: 22 } }}
                          choices={[
                            { value: '', label: 'All categories' },
                            ...catalogueCategories.map((category) => ({
                              value: category,
                              label: category,
                            })),
                          ]}
                          onChange={(event: Change) => {
                            setCatalogueCategory(String(event.value ?? ''));
                            setCatalogueLimit(CATALOGUE_PAGE_SIZE);
                          }}
                        />
                      </Row>
                    </Column>
                  ) : null}
                  {catalogueState === 'loading' && (
                    <Row gap={1} align="center">
                      <Spinner />
                      <Text label="Loading extension catalogue…" color="text-dim" />
                    </Row>
                  )}
                  {catalogueState === 'ready' && catalogueEntries.length === 0 && (
                    <InlineMessage
                      label="The built-in extension catalogue is currently empty."
                      width={COPY_WIDTH}
                      tone="neutral"
                    />
                  )}
                  {catalogueEntries.length > 0 && visibleCatalogueEntries.length === 0 ? (
                    <Column gap={1} align="start">
                      <InlineMessage
                        label="No extensions match this search and status filter."
                        tone="neutral"
                      />
                      <Button
                        label="Clear filters"
                        size="small"
                        onInvoke={() => {
                          setCatalogueQuery('');
                          setCatalogueFilter('discover');
                          setCatalogueCategory('');
                          setCatalogueLimit(CATALOGUE_PAGE_SIZE);
                        }}
                      />
                    </Column>
                  ) : null}
                  {visibleCatalogueEntries.length > 0 && (
                    <Row gap={1} width="fill" wrap>
                      {renderedCatalogueEntries.map((entry) => {
                        const compatibility = catalogueCompatibility(entry, workspaceArchitecture);
                        const trust = catalogueTrust(entry);
                        const installedExtension = installed.find(
                          (extension) => extension.name === entry.id,
                        );
                        const updateAvailable = Boolean(
                          installedExtension &&
                          newerVersion(entry.version, installedExtension.version),
                        );
                        const provider = installedExtension?.pane_providers?.[0];
                        return (
                          <Card
                            key={entry.id}
                            grow
                            width={{ minimum: { chars: 38 }, maximum: 'fill' }}
                            variant="outline"
                          >
                            <CardHeader
                              label={entry.title}
                              detail={`${entry.publisher} · Version ${entry.version}`}
                              align="start"
                              width="fill"
                            />
                            <CardContent gap={1}>
                              <Text label={entry.description} color="text-dim" wrap />
                              <Row gap={1} width="fill" wrap align="center" justify="start">
                                <Badge
                                  label={
                                    installedExtension
                                      ? `Installed · ${updateAvailable ? 'update available' : 'up to date'}`
                                      : 'Available'
                                  }
                                  tone={
                                    updateAvailable || !installedExtension ? 'accent' : 'positive'
                                  }
                                />
                                <Text label={trust.label} color="text-dim" />
                                {compatibility.compatible !== true ? (
                                  <Badge
                                    label={
                                      compatibility.compatible === false
                                        ? 'Incompatible'
                                        : 'Compatibility undeclared'
                                    }
                                    tone={compatibility.compatible === false ? 'danger' : 'warning'}
                                  />
                                ) : null}
                              </Row>
                              {compatibility.compatible !== true ? (
                                <Text
                                  label={compatibility.label}
                                  color={
                                    compatibility.compatible === false ? 'warning' : 'text-dim'
                                  }
                                  wrap
                                />
                              ) : null}
                              <Expander label="Trust & compatibility" expanded={false}>
                                <Column gap={1}>
                                  <Text
                                    label={`Catalogue source · ${entry.source}`}
                                    color="text-dim"
                                    wrap
                                  />
                                  <Text
                                    label={`Categories · ${(entry.categories ?? []).join(', ')}`}
                                    color="text-dim"
                                    wrap
                                  />
                                  <Text
                                    label={`Image · ${compactImageReference(entry.reference)}`}
                                    color="text-dim"
                                    tooltip={entry.reference}
                                    wrap
                                  />
                                  <Text
                                    label={`Protocol ${entry.protocol ?? 'unavailable'} · ${entry.architectures?.join(', ') || 'architecture unavailable'}`}
                                    color="text-dim"
                                    wrap
                                  />
                                  {installedExtension ? (
                                    <Text
                                      label={`Installed image · ${capitalize(extensionState(installedExtension))} · ${compactDigest(installedExtension.image_digest)}`}
                                      color="text-dim"
                                      tooltip={installedExtension.image_digest}
                                      wrap
                                    />
                                  ) : null}
                                </Column>
                              </Expander>
                            </CardContent>
                            <CardActions gap={1} align="start" justify="start" width="fill">
                              {updateAvailable ? (
                                <Button
                                  label="Review update"
                                  tooltip={`Review the ${entry.version} update for ${entry.title}`}
                                  size="small"
                                  variant="filled"
                                  tone="accent"
                                  enabled={!busy && compatibility.compatible !== false}
                                  onInvoke={() => inspect(entry.reference, entry)}
                                />
                              ) : !installedExtension ? (
                                <Button
                                  label="Review access"
                                  tooltip={`Review access requested by ${entry.title}`}
                                  size="small"
                                  variant="filled"
                                  tone="accent"
                                  enabled={!busy && compatibility.compatible !== false}
                                  onInvoke={() => inspect(entry.reference, entry)}
                                />
                              ) : (
                                <>
                                  {provider ? providerAction(installedExtension, provider) : null}
                                  <Button
                                    label="Check current image"
                                    tooltip={`Inspect ${entry.reference} again and compare its immutable digest`}
                                    size="small"
                                    variant="outline"
                                    enabled={!busy && compatibility.compatible !== false}
                                    onInvoke={() => inspect(entry.reference, entry)}
                                  />
                                </>
                              )}
                            </CardActions>
                          </Card>
                        );
                      })}
                    </Row>
                  )}
                  {visibleCatalogueEntries.length > renderedCatalogueEntries.length ? (
                    <Row gap={1} width="fill" wrap align="center" justify="start">
                      <Text
                        label={`Showing ${renderedCatalogueEntries.length} of ${visibleCatalogueEntries.length} matching extensions`}
                        color="text-dim"
                      />
                      <Button
                        label={`Show ${Math.min(CATALOGUE_PAGE_SIZE, visibleCatalogueEntries.length - renderedCatalogueEntries.length)} more`}
                        size="small"
                        onInvoke={() =>
                          setCatalogueLimit((current) => current + CATALOGUE_PAGE_SIZE)
                        }
                      />
                    </Row>
                  ) : null}
                  {catalogue && !catalogue.complete && (
                    <InlineMessage label="The built-in catalogue is incomplete." tone="warning" />
                  )}
                  {catalogueState === 'error' && (
                    <Column gap={1}>
                      <RecoveryState
                        operation="Extension catalogue"
                        error={catalogueError}
                        retryLabel="Retry catalogue"
                        onRetry={loadCatalogue}
                      />
                    </Column>
                  )}
                </Column>
              )}
              {!acquisition ? (
                <Expander label="Install from an OCI image" expanded={false}>
                  <Card grow={false} width="fill" variant="outline">
                    <CardContent>
                      <Row gap={1} width="fill" wrap>
                        <Entry
                          grow={false}
                          value={reference}
                          placeholder="registry.example/extension:version"
                          tooltip={
                            reference || 'Paste a full OCI image reference; press Enter to inspect'
                          }
                          width={{ chars: 24 }}
                          onChange={(event: Change) =>
                            setReference(String(event.value ?? '').slice(0, 512))
                          }
                          onSubmit={() => inspect()}
                        />
                        <Button
                          label={busy === 'inspect' ? 'Inspecting…' : 'Inspect'}
                          variant="filled"
                          tone="accent"
                          enabled={Boolean(reference.trim()) && !busy}
                          onInvoke={() => inspect()}
                        />
                      </Row>
                      <Text
                        label="Paste an OCI image reference. You’ll review compatibility and requested access before installation."
                        color="text-dim"
                        wrap
                      />
                    </CardContent>
                  </Card>
                </Expander>
              ) : (
                <Card grow={false} width="fill" variant="outline">
                  <CardHeader
                    label={
                      acquisition.candidate
                        ? `Review ${acquisition.candidate.name}`
                        : acquisition.state === 'failed'
                          ? 'Couldn’t inspect extension'
                          : acquisition.state === 'cancelled'
                            ? 'Inspection cancelled'
                            : 'Inspecting extension'
                    }
                    detail={
                      acquisition.candidate?.installed_image_digest
                        ? 'Update extension'
                        : acquisition.candidate
                          ? 'Install extension'
                          : 'Image inspection'
                    }
                    align="start"
                    width="fill"
                  />
                  {acquisition?.candidate && (
                    <CardContent gap={1}>
                      <Text
                        label={`${acquisition.candidate.name} · ${acquisition.candidate.version}`}
                      />
                      <Text
                        label={`Source ${compactImageReference(acquisition.reference)}`}
                        color="text-dim"
                        tooltip={acquisition.reference}
                        wrap
                      />
                      <Text
                        label={`Reviewed image ${compactDigest(acquisition.candidate.image_digest)}`}
                        tooltip={acquisition.candidate.image_digest}
                        wrap
                      />
                      {catalogueExpectation ? (
                        <Column gap={1}>
                          <Badge {...catalogueTrust(catalogueExpectation)} />
                          <Text
                            label={`Catalogue source · ${catalogueExpectation.source}`}
                            color="text-dim"
                            wrap
                          />
                        </Column>
                      ) : null}
                      {identityReviewWarning ? (
                        <InlineMessage label={identityReviewWarning} tone="warning" />
                      ) : null}
                      {catalogueMismatch ? (
                        <RecoveryState
                          operation="Catalogue verification"
                          error={`${catalogueMismatch} Return to the catalogue and review its latest entry before installing.`}
                          retryLabel="Back to catalogue"
                          onRetry={dismissReview}
                        />
                      ) : null}
                      <Heading label="Review permissions" scale="caption" />
                      <RequestedPermissionSummary groups={requestedPermissionGroups} />
                      <Text
                        label="All access is off. Expand exact grants and enable only what this extension needs."
                        color="text-dim"
                        wrap
                      />
                      {missingRequiredCapabilities.length > 0 ? (
                        <Column gap={1} align="start">
                          <InlineMessage
                            label={`${countLabel(missingRequiredCapabilities.length, 'required permission')} ${missingRequiredCapabilities.length === 1 ? 'is' : 'are'} off: ${missingRequiredCapabilities.map(capabilityLabel).join(', ')}. Optional access stays off.`}
                            tone="warning"
                          />
                          <Button
                            label="Select required access"
                            tooltip="Enable only the permissions required for this extension to remain available"
                            size="small"
                            variant="outline"
                            tone="accent"
                            enabled={!busy}
                            onInvoke={() =>
                              setGranted((current) => [
                                ...new Set([...current, ...requiredCapabilities]),
                              ])
                            }
                          />
                        </Column>
                      ) : null}
                      {privilegedAccessWarning ? (
                        <InlineMessage label={privilegedAccessWarning} tone="warning" />
                      ) : null}
                      <Expander
                        label={`Exact grants · ${grantedPermissionCount}/${requestedPermissionCount} selected`}
                        expanded={permissionDetailsExpanded}
                        onExpand={(event: Change) =>
                          setPermissionDetailsExpanded(Boolean(event.value))
                        }
                      >
                        <Column gap={1}>
                          {acquisition.candidate.requested.length > 0 && (
                            <Row gap={1} width="fill" align="center" justify="stretch">
                              <Text
                                label={`Product access · ${granted.length}/${acquisition.candidate.requested.length}`}
                                color="text-dim"
                              />
                              <Spacer />
                              {granted.length > 0 && (
                                <Button
                                  label="Clear product access"
                                  size="small"
                                  variant="ghost"
                                  onInvoke={() => {
                                    setGranted([]);
                                    setGrantedImages({
                                      read: [],
                                      use: [],
                                      pull: [],
                                      remove: [],
                                      prune_all_unused: false,
                                    });
                                    setGrantedContainers((current) => ({
                                      ...current,
                                      create: false,
                                    }));
                                    setGrantedNetworks((current) => ({
                                      ...current,
                                      create: false,
                                    }));
                                    setGrantedVolumes((current) => ({ ...current, create: false }));
                                    setGrantedFilesystem(emptyFilesystemGrant());
                                    setGrantedWorkspaceEnvironment({ read: [], write: [] });
                                  }}
                                />
                              )}
                            </Row>
                          )}
                          {acquisition.candidate.requested.map((capability) => (
                            <FormControlLabel
                              key={capability}
                              label={`${capabilityLabel(capability)}${requiredCapabilities.includes(capability) ? ' · Required' : ''}`}
                              tooltip={capability}
                              gap={2}
                            >
                              <Switch
                                checked={granted.includes(capability)}
                                onToggle={(event: Change) => {
                                  const enabled = Boolean(event.value);
                                  setGranted((current) =>
                                    withCapability(current, capability, enabled),
                                  );
                                  if (!enabled && capability === 'workspace-environment:read')
                                    setGrantedWorkspaceEnvironment((current) => ({
                                      ...current,
                                      read: [],
                                    }));
                                  if (!enabled && capability === 'workspace-environment:write')
                                    setGrantedWorkspaceEnvironment((current) => ({
                                      ...current,
                                      write: [],
                                    }));
                                  if (!enabled && capability === 'filesystem:read')
                                    setGrantedFilesystem((current) => ({ ...current, read: [] }));
                                  if (!enabled && capability === 'filesystem:write')
                                    setGrantedFilesystem((current) => ({
                                      ...current,
                                      write: [],
                                      create: [],
                                      delete: [],
                                      rename: [],
                                    }));
                                  if (!enabled && capability === 'images:read')
                                    setGrantedImages((current) => ({ ...current, read: [] }));
                                  if (!enabled && capability === 'images:pull')
                                    setGrantedImages((current) => ({ ...current, pull: [] }));
                                  if (!enabled && capability === 'images:remove')
                                    setGrantedImages((current) => ({ ...current, remove: [] }));
                                  if (!enabled && capability === 'images:prune')
                                    setGrantedImages((current) => ({
                                      ...current,
                                      prune_all_unused: false,
                                    }));
                                  if (!enabled && capability === 'containers:create') {
                                    setGrantedImages((current) => ({ ...current, use: [] }));
                                    setGrantedContainers((current) => ({
                                      ...current,
                                      create: false,
                                    }));
                                  }
                                  if (!enabled && capability === 'networks:write')
                                    setGrantedNetworks((current) => ({
                                      ...current,
                                      create: false,
                                    }));
                                  if (!enabled && capability === 'volumes:write')
                                    setGrantedVolumes((current) => ({ ...current, create: false }));
                                }}
                              />
                            </FormControlLabel>
                          ))}
                          {(requestedContainers.selectors.length > 0 ||
                            requestedContainers.create) && (
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
                                onToggle={(event: Change) => {
                                  const enabled = Boolean(event.value);
                                  setGranted((current) =>
                                    withCapability(
                                      current,
                                      'containers:create',
                                      enabled || grantedImages.use.length > 0,
                                    ),
                                  );
                                  setGrantedContainers((current) => ({
                                    ...current,
                                    create: enabled,
                                  }));
                                }}
                              />
                            </FormControlLabel>
                          )}
                          {imageGrantCount(requestedImages) > 0 && (
                            <>
                              <Text
                                label={`Image access · ${imageGrantCount(grantedImages)}/${imageGrantCount(requestedImages)}`}
                                color="text-dim"
                              />
                              <Text
                                label="Each image switch includes only the matching product action."
                                color="text-dim"
                                wrap
                              />
                            </>
                          )}
                          {IMAGE_VERBS.flatMap(({ key: verb, label }) =>
                            requestedImages[verb].map((selector) => {
                              const key = imageSelectorKey(selector);
                              const selected = grantedImages[verb].some(
                                (candidate) => imageSelectorKey(candidate) === key,
                              );
                              return (
                                <FormControlLabel
                                  key={`${verb}:${key}`}
                                  label={`${label} · ${imageSelectorLabel(selector)}`}
                                  gap={2}
                                >
                                  <Switch
                                    checked={selected}
                                    onToggle={(event: Change) => {
                                      const next = {
                                        ...grantedImages,
                                        [verb]: event.value
                                          ? grantedImages[verb].some(
                                              (candidate) => imageSelectorKey(candidate) === key,
                                            )
                                            ? grantedImages[verb]
                                            : [...grantedImages[verb], selector]
                                          : grantedImages[verb].filter(
                                              (candidate) => imageSelectorKey(candidate) !== key,
                                            ),
                                      };
                                      const capability = imageCapability(verb);
                                      const enabled =
                                        next[verb].length > 0 ||
                                        (capability === 'containers:create' &&
                                          grantedContainers.create);
                                      setGranted((current) =>
                                        withCapability(current, capability, enabled),
                                      );
                                      setGrantedImages(next);
                                    }}
                                  />
                                </FormControlLabel>
                              );
                            }),
                          )}
                          {requestedImages.prune_all_unused && (
                            <FormControlLabel label="Prune every unused image" gap={2}>
                              <Switch
                                checked={grantedImages.prune_all_unused}
                                onToggle={(event: Change) => {
                                  const enabled = Boolean(event.value);
                                  setGranted((current) =>
                                    withCapability(current, 'images:prune', enabled),
                                  );
                                  setGrantedImages((current) => ({
                                    ...current,
                                    prune_all_unused: enabled,
                                  }));
                                }}
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
                              <FormControlLabel
                                key={key}
                                label={networkSelectorLabel(selector)}
                                gap={2}
                              >
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
                                onToggle={(event: Change) => {
                                  const enabled = Boolean(event.value);
                                  setGranted((current) =>
                                    withCapability(current, 'networks:write', enabled),
                                  );
                                  setGrantedNetworks((current) => ({
                                    ...current,
                                    create: enabled,
                                  }));
                                }}
                              />
                            </FormControlLabel>
                          )}
                          {(requestedVolumes.selectors.length > 0 || requestedVolumes.create) && (
                            <Text
                              label={`Volume access · ${grantedVolumes.selectors.length + Number(grantedVolumes.create)}/${requestedVolumes.selectors.length + Number(requestedVolumes.create)}`}
                              color="text-dim"
                            />
                          )}
                          {requestedVolumes.selectors.map((selector) => {
                            const key = volumeSelectorKey(selector);
                            const selected = grantedVolumes.selectors.some(
                              (candidate) => volumeSelectorKey(candidate) === key,
                            );
                            return (
                              <FormControlLabel
                                key={key}
                                label={volumeSelectorLabel(selector)}
                                gap={2}
                              >
                                <Switch
                                  checked={selected}
                                  onToggle={(event: Change) =>
                                    setGrantedVolumes((current) => ({
                                      ...current,
                                      selectors: event.value
                                        ? [
                                            ...current.selectors.filter(
                                              (candidate) => volumeSelectorKey(candidate) !== key,
                                            ),
                                            selector,
                                          ]
                                        : current.selectors.filter(
                                            (candidate) => volumeSelectorKey(candidate) !== key,
                                          ),
                                    }))
                                  }
                                />
                              </FormControlLabel>
                            );
                          })}
                          {requestedVolumes.create && (
                            <FormControlLabel label="Create new volumes" gap={2}>
                              <Switch
                                checked={grantedVolumes.create}
                                onToggle={(event: Change) => {
                                  const enabled = Boolean(event.value);
                                  setGranted((current) =>
                                    withCapability(current, 'volumes:write', enabled),
                                  );
                                  setGrantedVolumes((current) => ({ ...current, create: enabled }));
                                }}
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
                                onCapabilityChange={(capability, enabled) =>
                                  setGranted((current) =>
                                    withCapability(current, capability, enabled),
                                  )
                                }
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
                                (candidate) =>
                                  JSON.stringify(candidate) === JSON.stringify(selector),
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
                                    onToggle={(event: Change) => {
                                      const enabled = Boolean(event.value);
                                      const selected = enabled
                                        ? [...grantedWorkspaceEnvironment[verb], selector]
                                        : grantedWorkspaceEnvironment[verb].filter(
                                            (candidate) =>
                                              JSON.stringify(candidate) !==
                                              JSON.stringify(selector),
                                          );
                                      setGrantedWorkspaceEnvironment((current) => ({
                                        ...current,
                                        [verb]: selected,
                                      }));
                                      setGranted((capabilities) =>
                                        withCapability(
                                          capabilities,
                                          verb === 'read'
                                            ? 'workspace-environment:read'
                                            : 'workspace-environment:write',
                                          selected.length > 0,
                                        ),
                                      );
                                    }}
                                  />
                                </FormControlLabel>
                              );
                            }),
                          )}
                        </Column>
                      </Expander>
                    </CardContent>
                  )}
                  {acquisition && acquisition.state !== 'ready' && (
                    <CardContent gap={1}>
                      <Text
                        label={`Source ${compactImageReference(acquisition.reference)}`}
                        color="text-dim"
                        tooltip={acquisition.reference}
                        wrap
                      />
                      {acquisition.state === 'failed' ? (
                        <Column gap={1}>
                          <InlineMessage
                            label={acquisitionFailure(
                              acquisition.error ?? 'The image could not be inspected.',
                            )}
                            tone="danger"
                            width={COPY_WIDTH}
                          />
                          <Row gap={1} wrap>
                            <Button
                              label="Retry inspection"
                              variant="filled"
                              tone="accent"
                              enabled={!busy}
                              onInvoke={() => inspect(reference, catalogueExpectation)}
                            />
                            <Button
                              label="Back to catalogue"
                              variant="ghost"
                              enabled={!busy}
                              onInvoke={dismissReview}
                            />
                          </Row>
                          <Expander label="Technical details" expanded={false} width="fill">
                            <Text
                              label={acquisitionTechnicalDetail(
                                acquisition.error ?? 'The image could not be inspected.',
                              )}
                              wrap
                            />
                          </Expander>
                        </Column>
                      ) : acquisition.state === 'cancelled' ? (
                        <Column gap={1}>
                          <Text label={acquisitionLabel(acquisition)} color="text-dim" wrap />
                          <Button
                            label="Back to catalogue"
                            variant="ghost"
                            enabled={!busy}
                            onInvoke={dismissReview}
                          />
                        </Column>
                      ) : acquisition.state === 'committing' ? (
                        <Row gap={1} align="center" wrap>
                          <Spinner />
                          <Text label={acquisitionLabel(acquisition)} wrap />
                        </Row>
                      ) : ['installed', 'updated'].includes(acquisition.state) ? (
                        <Text label={acquisitionLabel(acquisition)} wrap />
                      ) : (
                        <Row gap={1} width="fill" align="center" justify="stretch">
                          <Column gap={1} grow>
                            <Row gap={1} align="center" wrap>
                              {acquisition.progress ? null : <Spinner />}
                              <Text label={acquisitionLabel(acquisition)} wrap />
                            </Row>
                            {acquisition.progress ? (
                              <Progress
                                fraction={acquisitionProgressFraction(acquisition)}
                                tooltip={acquisitionLabel(acquisition)}
                                width="fill"
                              />
                            ) : null}
                          </Column>
                          <Spacer />
                          <Button
                            label={busy === 'cancel' ? 'Cancelling…' : 'Cancel inspection'}
                            variant="outline"
                            size="small"
                            enabled={busy !== 'cancel'}
                            onInvoke={cancel}
                          />
                        </Row>
                      )}
                    </CardContent>
                  )}
                </Card>
              )}
            </Column>
          ) : (
            <Column gap={2} width="fill" height="content">
              <Row gap={1} width="fill" align="center" justify="stretch">
                <Row gap={1} align="center" wrap>
                  <Heading
                    label="Installed extensions"
                    scale="caption"
                    grow={false}
                    align="start"
                  />
                  {inventoryState !== 'loading' ? (
                    <Badge label={countLabel(installed.length, 'extension')} />
                  ) : null}
                </Row>
                <Spacer />
                <IconButton
                  label="Refresh installed extensions"
                  icon="view-refresh-symbolic"
                  size="small"
                  variant="ghost"
                  enabled={!busy && inventoryState !== 'loading'}
                  onInvoke={reload}
                />
              </Row>
              {watchError && (
                <RecoveryState
                  operation="Extension updates"
                  error={watchError}
                  retryLabel="Refresh extensions"
                  onRetry={reload}
                />
              )}
              {inventoryState === 'ready' ? (
                <Column gap={1} width="fill">
                  <Text label="Find installed extensions" color="text-dim" />
                  <Row gap={1} width="fill" wrap align="center" justify="start">
                    <Search
                      grow
                      value={installedQuery}
                      placeholder="Search installed"
                      tooltip="Search by extension name, runtime status, or interface provider"
                      width={{ minimum: { chars: 18 }, maximum: { chars: 36 } }}
                      onChange={(event: Change) => {
                        setInstalledQuery(String(event.value ?? '').slice(0, 128));
                        setInstalledLimit(INSTALLED_PAGE_SIZE);
                      }}
                    />
                    <Select
                      value={installedFilter}
                      tooltip="Filter installed extensions by status"
                      width={{ minimum: { chars: 22 }, maximum: { chars: 22 } }}
                      choices={[
                        { value: 'all', label: 'All installed' },
                        { value: 'running', label: 'Running' },
                        { value: 'faulted', label: 'Faulted' },
                        { value: 'updates', label: 'Updates' },
                        { value: 'disabled', label: 'Disabled' },
                      ]}
                      onChange={(event: Change) => {
                        const selected = String(event.value ?? '');
                        if (
                          ['all', 'running', 'faulted', 'updates', 'disabled'].includes(selected)
                        ) {
                          setInstalledFilter(selected as InstalledFilter);
                          setInstalledLimit(INSTALLED_PAGE_SIZE);
                        }
                      }}
                    />
                    <Text
                      label={`${visibleInstalled.length} of ${countLabel(installed.length, 'installed extension')}`}
                      color="text-dim"
                    />
                  </Row>
                </Column>
              ) : null}
              <ResourceState
                state={inventoryState}
                loadingLabel="Loading installed extensions…"
                emptyLabel="No extensions installed"
                emptyDetail="Choose an extension or inspect an OCI image."
                error={inventoryError || 'Installed extensions could not be loaded.'}
                onRetry={reload}
              >
                {visibleInstalled.length === 0 ? (
                  <Column gap={1} align="start">
                    <InlineMessage
                      label="No installed extensions match this search and status filter."
                      tone="neutral"
                    />
                    <Button
                      label="Clear installed filters"
                      size="small"
                      onInvoke={() => {
                        setInstalledQuery('');
                        setInstalledFilter('all');
                        setInstalledLimit(INSTALLED_PAGE_SIZE);
                      }}
                    />
                  </Column>
                ) : (
                  <Row gap={1} width="fill" wrap>
                    {renderedInstalled.map((extension) => {
                      const catalogueEntry = catalogue?.entries.find(
                        (entry) => entry.id === extension.name,
                      );
                      const update =
                        catalogueEntry && newerVersion(catalogueEntry.version, extension.version)
                          ? catalogueEntry
                          : undefined;
                      const updateCompatibility = update
                        ? catalogueCompatibility(update, workspaceArchitecture)
                        : null;
                      const currentCompatibility = catalogueEntry
                        ? catalogueCompatibility(catalogueEntry, workspaceArchitecture)
                        : null;
                      const provider = extension.pane_providers?.[0];
                      const hasCardAction = Boolean(
                        update ||
                        (extension.name !== 'top' &&
                          (extension.status.startsWith('fault:') || !extension.enabled)) ||
                        provider ||
                        catalogueEntry,
                      );
                      return (
                        <Card
                          key={`${extension.name}:${extension.image_digest}`}
                          grow
                          width={{ chars: 36 }}
                          height="content"
                          variant="outline"
                        >
                          <CardContent gap={1}>
                            <Row gap={1} width="fill" align="center" justify="start" wrap>
                              <Column gap={0} grow>
                                <Text label={extension.name} tooltip={extension.image_digest} />
                                <Text
                                  label={
                                    extension.version
                                      ? `Version ${extension.version}`
                                      : 'Version unavailable'
                                  }
                                  color="text-dim"
                                />
                              </Column>
                              <Badge
                                label={capitalize(extensionState(extension))}
                                tone={
                                  extension.status.startsWith('fault:')
                                    ? 'danger'
                                    : extension.enabled
                                      ? 'positive'
                                      : 'neutral'
                                }
                              />
                            </Row>
                            <ExtensionFault extension={extension} />
                            {updateCompatibility ? (
                              <Text
                                label={
                                  update
                                    ? updateCompatibility.compatible === true
                                      ? `Update available · Version ${update.version}`
                                      : `Update to Version ${update.version} · ${updateCompatibility.label}`
                                    : `Update · ${updateCompatibility.label}`
                                }
                                color={
                                  updateCompatibility.compatible === false ? 'warning' : 'text-dim'
                                }
                                wrap
                              />
                            ) : null}
                            <LifecycleFeedback
                              extensionName={extension.name}
                              pending={pendingLifecycle}
                              failure={lifecycleFailure}
                            />
                            {extension.name === 'top' ? (
                              <InstalledPermissionSummary extension={extension} />
                            ) : (
                              <Expander label="View permissions" expanded={false}>
                                <InstalledPermissionSummary extension={extension} />
                              </Expander>
                            )}
                            {hasCardAction || extension.name !== 'top' ? (
                              <Row gap={1} width="fill" align="center" justify="start" wrap>
                                {update && (
                                  <Button
                                    key="review-update"
                                    label="Review update"
                                    size="small"
                                    variant="filled"
                                    tone="accent"
                                    enabled={!busy && updateCompatibility?.compatible !== false}
                                    onInvoke={() => inspect(update.reference, update)}
                                  />
                                )}
                                {!update &&
                                extension.name !== 'top' &&
                                extension.status.startsWith('fault:') ? (
                                  <Button
                                    key="lifecycle"
                                    label="Retry"
                                    size="small"
                                    variant="outline"
                                    tone="accent"
                                    enabled={!busy}
                                    onInvoke={() => lifecycle(extension, 'retry')}
                                  />
                                ) : !update && extension.name !== 'top' && !extension.enabled ? (
                                  <Button
                                    key="lifecycle"
                                    label="Enable"
                                    size="small"
                                    variant="outline"
                                    tone="accent"
                                    enabled={!busy}
                                    onInvoke={() => lifecycle(extension, 'enable')}
                                  />
                                ) : !update && provider ? (
                                  providerAction(extension, provider)
                                ) : null}
                                {!update && catalogueEntry ? (
                                  <Button
                                    label="Check for changes"
                                    tooltip={`Check ${extension.name} image for changes`}
                                    size="small"
                                    variant="ghost"
                                    enabled={!busy && currentCompatibility?.compatible !== false}
                                    onInvoke={() =>
                                      inspect(catalogueEntry.reference, catalogueEntry)
                                    }
                                  />
                                ) : null}
                                {extension.enabled && !extension.status.startsWith('fault:') ? (
                                  <Button
                                    label="Disable"
                                    size="small"
                                    variant="ghost"
                                    enabled={!busy}
                                    onInvoke={() => lifecycle(extension, 'disable')}
                                  />
                                ) : null}
                                <ConfirmAction
                                  label="Remove"
                                  confirmLabel={`Remove ${extension.name}`}
                                  question={`Remove ${extension.name} and permanently delete its private workspace data?`}
                                  authorityKey={extension.image_digest}
                                  enabled={!busy}
                                  size="small"
                                  onConfirm={() => lifecycle(extension, 'remove')}
                                />
                              </Row>
                            ) : null}
                          </CardContent>
                        </Card>
                      );
                    })}
                  </Row>
                )}
                {visibleInstalled.length > renderedInstalled.length ? (
                  <Row gap={1} width="fill" wrap align="center" justify="start">
                    <Text
                      label={`Showing ${renderedInstalled.length} of ${visibleInstalled.length} matching installed extensions`}
                      color="text-dim"
                    />
                    <Button
                      label={`Show ${Math.min(INSTALLED_PAGE_SIZE, visibleInstalled.length - renderedInstalled.length)} more installed`}
                      size="small"
                      onInvoke={() => setInstalledLimit((current) => current + INSTALLED_PAGE_SIZE)}
                    />
                  </Row>
                ) : null}
              </ResourceState>
            </Column>
          )}
        </Column>
      </Container>
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
              label={
                requestedPermissionCount > 0 && grantedPermissionCount === 0
                  ? `No access selected · ${requestedPermissionCount} requested`
                  : `Review decision · ${grantedPermissionCount}/${requestedPermissionCount} selected`
              }
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
              enabled={
                !busy &&
                acquisition.state === 'ready' &&
                missingRequiredCapabilities.length === 0 &&
                !catalogueMismatch
              }
              variant="filled"
              tone="accent"
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

function volumeSelectorKey(selector: VolumeSelector): string {
  return 'all' in selector ? 'all' : `name:${selector.name}`;
}

function volumeSelectorLabel(selector: VolumeSelector): string {
  return 'all' in selector ? 'All workspace volumes' : `Volume named ${selector.name}`;
}

function imageSelectorKey(selector: ImageSelector): string {
  if ('digest' in selector) return `digest:${selector.digest}`;
  if ('reference' in selector) return `reference:${selector.reference}`;
  return 'all';
}

function imageSelectorLabel(selector: ImageSelector): string {
  if ('digest' in selector) return selector.digest;
  if ('reference' in selector) return selector.reference;
  return 'All images';
}

function imageGrantCount(grant: ImageGrant): number {
  return (
    grant.read.length +
    grant.use.length +
    grant.pull.length +
    grant.remove.length +
    Number(grant.prune_all_unused)
  );
}

export function acquisitionLabel(acquisition: ExtensionAcquisitionStatus): string {
  const progress = acquisition.progress;
  if (!progress) {
    const labels: Record<string, string> = {
      queued: 'Waiting for an acquisition worker…',
      inspecting: 'Checking whether the image is available for this workspace architecture…',
      'reading-manifest': 'Reading and validating the extension manifest…',
      ready: 'Manifest validated. Review requested access before installing.',
      committing: 'Saving the reviewed extension and its granted access…',
      installed: 'Extension installed.',
      updated: 'Extension updated.',
      failed: 'Inspection failed. The reference and details are retained for retry.',
      cancelled: 'Inspection cancelled. No extension was installed.',
    };
    return labels[acquisition.state] ?? 'Inspecting image…';
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

export function acquisitionProgressFraction(
  acquisition: ExtensionAcquisitionStatus,
): number | undefined {
  const current = acquisition.progress?.current;
  const total = acquisition.progress?.total;
  if (
    current === null ||
    current === undefined ||
    total === null ||
    total === undefined ||
    total <= 0
  )
    return undefined;
  return Math.min(1, Math.max(0, current / total));
}

export function acquisitionFailure(detail: string): string {
  const normalized = detail.replaceAll('\\n', ' ').replaceAll(/\s+/g, ' ').trim();
  const registryMessage = /"message"\s*:\s*"([^"]+)"/.exec(normalized)?.[1];
  if (/unauthorized|denied|authentication required|insufficient_scope/i.test(normalized)) {
    return 'Registry access denied. Sign in with credentials that can read this image, or verify that the image is public.';
  }
  if (registryMessage) {
    return `The registry could not provide this image: ${registryMessage}. Verify the image name, version, and visibility.`.slice(
      0,
      300,
    );
  }
  if (/requires linux\/|but this workspace requires linux\//i.test(normalized)) {
    return `Architecture mismatch: ${normalized} Choose an image published for this workspace architecture.`.slice(
      0,
      300,
    );
  }
  if (/manifest/i.test(normalized)) {
    return `Extension manifest could not be validated: ${normalized} Check the image's Husklet manifest label and protocol version.`.slice(
      0,
      300,
    );
  }
  if (/workspace (execution domain|resources) (failed|unavailable)|Engine\(/i.test(normalized)) {
    return `Workspace image service is unavailable. Reopen the workspace resources, then retry inspection. ${normalized}`.slice(
      0,
      300,
    );
  }
  return normalized.slice(0, 300);
}

export function acquisitionTechnicalDetail(detail: string): string {
  const normalized = detail.replaceAll('\\n', '\n').trim();
  if (normalized.length <= 4_096) return normalized;
  return `${normalized.slice(0, 4_096)}\n… technical detail truncated`;
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

function lifecycleStateObserved(
  action: LifecycleAction,
  expected: ExtensionSummary,
  current: ExtensionSummary | undefined,
): boolean {
  if (action === 'remove') {
    return !current || current.image_digest !== expected.image_digest;
  }
  if (!current || current.image_digest !== expected.image_digest) return false;
  if (action === 'disable') return current.enabled !== true;
  if (action === 'enable') return current.enabled === true;
  return current.enabled === true && current.status === 'duty';
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
  return <RecoveryState operation="Extension" error={detail} />;
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
      <RecoveryState operation={`${capitalize(failure.action)} extension`} error={failure.detail} />
    );
  return null;
}

function capitalize(value: string): string {
  return `${value[0].toUpperCase()}${value.slice(1)}`;
}

function newerVersion(candidate: string, installed?: string): boolean {
  if (!installed) return true;
  const parse = (version: string) => {
    const match = /^(\d+)\.(\d+)\.(\d+)$/.exec(version);
    return match ? match.slice(1).map(Number) : null;
  };
  const next = parse(candidate);
  const current = parse(installed);
  if (!next || !current) return false;
  for (let index = 0; index < 3; index += 1) {
    if (next[index] !== current[index]) return next[index] > current[index];
  }
  return false;
}

export function capabilityLabel(capability: ExtensionCapability): string {
  const known: Record<ExtensionCapability, string> = {
    'workspaces:read': 'View workspace settings',
    'workspaces:configure': 'Modify workspace settings',
    'workspaces:control': 'Create, start, stop, and delete workspaces',
    'workspaces:events': 'Observe workspace lifecycle',
    'workspace-environment:read': 'Read selected workspace environment values',
    'workspace-environment:write': 'Change selected workspace environment values',
    'extensions:read': 'View installed extensions',
    'extensions:control': 'Enable, disable, and retry extensions',
    'extensions:install': 'Install and update extensions',
    'extensions:remove': 'Remove extensions',
    'containers:read': 'View containers and processes',
    'containers:create': 'Create containers from consented images',
    'containers:execute': 'Run and control detached processes in containers',
    'containers:input': 'Write to detached process input',
    'containers:lifecycle': 'Start, stop, pause, restart, rename, and signal containers',
    'containers:remove': 'Permanently remove containers',
    'containers:attach': 'Run commands inside containers',
    'images:read': 'View images',
    'images:pull': 'Pull images',
    'images:remove': 'Remove images',
    'images:prune': 'Prune every unused image',
    'volumes:read': 'View volumes',
    'volumes:write': 'Create and remove volumes',
    'networks:read': 'View networks',
    'networks:write': 'Create and modify networks',
    'terminals:read': 'View terminal tabs and panes',
    'terminals:input': 'Type into terminal panes',
    'terminals:focus': 'Move keyboard focus between terminal panes',
    'terminals:layout-control': 'Create and rearrange terminal panes',
    'terminals:process-control': 'Replace processes in terminal panes',
    'terminals:output': 'Read terminal text',
    'panes:observe': 'Observe pane interaction',
    'panes:semantic-read': 'Read structured pane interfaces',
    'panes:semantic-control': 'Operate structured pane interfaces',
    'interface:render': 'Render this extension interface',
    'filesystem:read': 'Read selected workspace files',
    'filesystem:write': 'Change selected workspace files',
    'state:read': 'Read this extension’s private state',
    'state:write': 'Change this extension’s private state',
    'preferences:read': 'Read this extension’s interface preferences',
    'preferences:write': 'Change this extension’s interface preferences',
    'credentials:read': 'Read selected extension credentials',
    'credentials:inject': 'Inject selected credentials into container processes',
    'credentials:write': 'Change selected extension credentials',
    'notifications:publish': 'Show workspace notifications',
  };
  return known[capability];
}

function compactDigest(digest: string): string {
  return digest.length > 32 ? `${digest.slice(0, 19)}…${digest.slice(-8)}` : digest;
}
