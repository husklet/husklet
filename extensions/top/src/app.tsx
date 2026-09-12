import React from 'react';
import {
  Row,
  Responsive,
  Select,
  Text,
  type ContainerSummary,
  type ExecutionSummary,
  type ExtensionSummary,
  type HostEvent,
  type ImageSummary,
  type NetworkSummary,
  type TabSummary,
  type VolumeSummary,
  type WorkspaceApi,
} from '@husklet/react';
import {
  ContainerDetailsSource,
  ExecutionDetailsSource,
  ImageDetailsSource,
  ProcessTableSource,
  VolumeDetailsSource,
} from './model.js';
import {
  availableSections,
  Navigation,
  Overview,
  type Resource,
  type Section,
} from './overview.js';
import { Terminals } from './terminals.js';
import { Processes } from './processes.js';
import { Executions } from './executions.js';
import { Images } from './images.js';
import { Volumes } from './volumes.js';
import { Networks } from './networks.js';
import { Containers } from './containers.js';
import { Workspace } from './workspace.js';
import { Extensions } from './extensions.js';

export { availableSections, Overview, SECTIONS } from './overview.js';
export { Terminals } from './terminals.js';
export { Processes } from './processes.js';
export { Executions } from './executions.js';
export { Images } from './images.js';
export { Volumes } from './volumes.js';
export { Networks } from './networks.js';
export { ContainerRename } from './container-rename.js';
export {
  ContainerCreate,
  parseArguments,
  parseLabels,
  parseMounts,
  parsePorts,
} from './container-create.js';
export { ContainerDetail } from './container-detail.js';
export { Containers } from './containers.js';
export { Workspace } from './workspace.js';
export {
  Extensions,
  acquisitionFailure,
  acquisitionTechnicalDetail,
  acquisitionLabel,
  acquisitionProgressFraction,
  capabilityLabel,
  catalogueTrust,
  catalogueCandidateMismatch,
  compactImageReference,
  filterCatalogueEntries,
  filterInstalledExtensions,
  installedExtensionNeedsAttention,
} from './extensions.js';

const { useCallback, useEffect, useMemo, useRef, useState } = React;
export const SIDEBAR_WIDTH_KEY = 'sidebar.width';
export const SIDEBAR_WIDTH_MIN = 144;
export const SIDEBAR_WIDTH_MAX = 240;
export const SIDEBAR_WIDTH_DEFAULT = 160;
export const SIDEBAR_SAVE_DELAY_MS = 250;

export function boundedSidebarWidth(value: unknown): number | null {
  if (typeof value !== 'number' || !Number.isSafeInteger(value)) return null;
  return Math.min(SIDEBAR_WIDTH_MAX, Math.max(SIDEBAR_WIDTH_MIN, value));
}

export async function persistSidebarWidth(api: WorkspaceApi, width: number): Promise<number> {
  const bounded = boundedSidebarWidth(width);
  if (bounded === null) throw new RangeError('sidebar width must be a safe integer');
  let last: unknown;
  for (let attempt = 0; attempt < 2; attempt += 1) {
    const current = await api.preferences.read();
    try {
      return await api.preferences.set(current.revision, SIDEBAR_WIDTH_KEY, {
        kind: 'number',
        value: bounded,
      });
    } catch (error) {
      if ((error as { kind?: unknown })?.kind !== 'conflict') throw error;
      last = error;
    }
  }
  throw last;
}
type Selections = { subscribe(listener: (event: HostEvent) => void): (() => void) | undefined };
type TopProps = {
  api: WorkspaceApi;
  selections?: Selections;
  containerDetails?: ContainerDetailsSource;
  executionDetails?: ExecutionDetailsSource;
  imageDetails?: ImageDetailsSource;
  processTable?: ProcessTableSource;
  volumeDetails?: VolumeDetailsSource;
  initial?: Partial<{
    containers: ContainerSummary[];
    executions: ExecutionSummary[];
    images: ImageSummary[];
    volumes: VolumeSummary[];
    networks: NetworkSummary[];
    terminals: TabSummary[];
    extensions: ExtensionSummary[];
  }>;
  initialSection?: Section;
};
export function Top({
  api,
  selections,
  containerDetails,
  executionDetails,
  imageDetails,
  processTable,
  volumeDetails,
  initial = {},
  initialSection = 'workspace',
}: TopProps) {
  const sections = useMemo(
    () => availableSections(api.grantedCapabilities),
    [api.grantedCapabilities],
  );
  // A provider deep-link may intentionally open a denied page so its recovery
  // action can explain and repair authority. Navigation itself lists only
  // destinations the current installation can use.
  const [section, setSection] = useState<Section>(initialSection);
  const compactSections = sections.includes(section) ? sections : [section, ...sections];
  const [sidebarWidth, setSidebarWidth] = useState(SIDEBAR_WIDTH_DEFAULT);
  const persistedSidebarWidth = useRef<number | null>(null);
  const sidebarWasResized = useRef(false);
  const [preferencesReady, setPreferencesReady] = useState(false);
  useEffect(() => {
    if (!api.preferences) return undefined;
    let live = true;
    void api.preferences
      .read()
      .then((preferences) => {
        const entry = preferences.entries.find(([key]) => key === SIDEBAR_WIDTH_KEY)?.[1];
        const width = entry?.kind === 'number' ? boundedSidebarWidth(entry.value) : null;
        if (!live) return;
        if (width !== null && !sidebarWasResized.current) setSidebarWidth(width);
        persistedSidebarWidth.current = width ?? SIDEBAR_WIDTH_DEFAULT;
        setPreferencesReady(true);
      })
      .catch(() => {
        if (!live) return;
        persistedSidebarWidth.current = SIDEBAR_WIDTH_DEFAULT;
        setPreferencesReady(true);
      });
    return () => {
      live = false;
    };
  }, [api]);
  useEffect(() => {
    if (!preferencesReady || sidebarWidth === persistedSidebarWidth.current) return undefined;
    const timer = setTimeout(() => {
      void persistSidebarWidth(api, sidebarWidth)
        .then(() => {
          persistedSidebarWidth.current = sidebarWidth;
        })
        .catch(() => {
          // A later resize retries; preference failure must not disable navigation.
        });
    }, SIDEBAR_SAVE_DELAY_MS);
    return () => clearTimeout(timer);
  }, [api, preferencesReady, sidebarWidth]);
  const [requestedExecution, setRequestedExecution] = useState('');
  const containers = useResource(api.containers.list, initial.containers);
  const images = useResource(api.images.list, initial.images);
  const volumes = useResource(api.volumes.list, initial.volumes);
  const networks = useResource(api.networks.list, initial.networks);
  const terminals = useResource(api.terminal?.tabs ?? (async () => []), initial.terminals);
  const extensions = useResource(api.extensions.list, initial.extensions);
  const [executionsTruncated, setExecutionsTruncated] = useState(false);
  const listExecutions = useCallback(async () => {
    const listing = await api.containers.executions();
    setExecutionsTruncated(listing.truncated);
    return listing.executions;
  }, [api]);
  const executions = useResource(listExecutions, initial.executions);
  const replaceExecutions = executions.replace;
  useEffect(() => {
    if (section !== 'executions' || typeof api.watchExecutions !== 'function') return undefined;
    let disposed = false;
    let stop: (() => void) | null = null;
    void api
      .watchExecutions((listing) => {
        if (disposed) return;
        setExecutionsTruncated(listing.truncated);
        replaceExecutions(listing.executions);
      })
      .then((dispose) => {
        if (disposed) void dispose();
        else stop = dispose;
      })
      .catch(() => {
        /* Explicit Refresh remains available when observation is unsupported. */
      });
    return () => {
      disposed = true;
      if (stop) void stop();
    };
  }, [api, section, replaceExecutions]);
  const reloadContainers = containers.reload;
  const reloadImages = images.reload;
  const reloadVolumes = volumes.reload;
  const reloadNetworks = networks.reload;
  const reloadTerminals = terminals.reload;
  useEffect(
    () =>
      selections?.subscribe((event) => {
        if ('pane_provider' in event && sections.includes(event.pane_provider as Section))
          setSection(event.pane_provider as Section);
        if ('snapshot' in event && event.snapshot === 'containers') void reloadContainers();
        if ('snapshot' in event && event.snapshot === 'images') void reloadImages();
        if ('snapshot' in event && event.snapshot === 'volumes') void reloadVolumes();
        if ('snapshot' in event && event.snapshot === 'networks') void reloadNetworks();
        if ('snapshot' in event && event.snapshot === 'terminal') void reloadTerminals();
      }),
    [
      selections,
      sections,
      reloadContainers,
      reloadImages,
      reloadVolumes,
      reloadNetworks,
      reloadTerminals,
    ],
  );
  useEffect(() => {
    if (typeof api.subscribe !== 'function') return undefined;
    void api.subscribe('containers');
    void api.subscribe('images');
    void api.subscribe('volumes');
    void api.subscribe('networks');
    void api.subscribe('terminal');
    return () => {
      if (typeof api.unsubscribe === 'function') {
        void api.unsubscribe('containers');
        void api.unsubscribe('images');
        void api.unsubscribe('volumes');
        void api.unsubscribe('networks');
        void api.unsubscribe('terminal');
      }
    };
  }, [api]);
  const body =
    section === 'workspace' ? (
      <Overview
        containers={containers}
        executions={executions}
        images={images}
        volumes={volumes}
        networks={networks}
        terminals={terminals}
        extensions={extensions}
        onOpen={setSection}
      />
    ) : section === 'settings' ? (
      <Workspace api={api} />
    ) : section === 'extensions' ? (
      <Extensions api={api} />
    ) : section === 'containers' ? (
      <Containers
        api={api}
        resource={containers}
        containerDetails={containerDetails}
        onOpenExecution={async (id: string) => {
          setRequestedExecution(id);
          await executions.reload();
          setSection('executions');
        }}
        onOpenExtensions={() => setSection('extensions')}
      />
    ) : section === 'processes' ? (
      <Processes
        api={api}
        resource={containers}
        processTable={processTable}
        onOpenContainers={() => setSection('containers')}
      />
    ) : section === 'executions' ? (
      <Executions
        api={api}
        resource={executions}
        executionDetails={executionDetails}
        truncated={executionsTruncated}
        requestedExecution={requestedExecution}
        onOpenContainers={() => setSection('containers')}
      />
    ) : section === 'images' ? (
      <Images api={api} resource={images} imageDetails={imageDetails} />
    ) : section === 'volumes' ? (
      <Volumes
        api={api}
        resource={volumes}
        volumeDetails={volumeDetails}
        onOpenExtensions={() => setSection('extensions')}
      />
    ) : section === 'networks' ? (
      <Networks
        api={api}
        resource={networks}
        containers={containers}
        onOpenExtensions={() => setSection('extensions')}
      />
    ) : (
      <Terminals api={api} resource={terminals} />
    );
  return (
    <Responsive
      grow
      orientation="horizontal"
      position={sidebarWidth}
      breakpoint={640}
      onChange={(event) => {
        const position = boundedSidebarWidth(Number(event.value));
        if (position !== null) {
          sidebarWasResized.current = true;
          setSidebarWidth(position);
        }
      }}
    >
      <Row width="fill" pad={1} gap={1} align="center" justify="start">
        <Text label="Section" color="text-dim" />
        <Select
          width={{ minimum: { chars: 14 }, maximum: { chars: 24 } }}
          value={section}
          choices={compactSections.map((name) => ({ value: name, label: sectionTitle(name) }))}
          onChange={(event) => {
            const selected = String(event.value ?? '');
            if (compactSections.includes(selected as Section)) setSection(selected as Section);
          }}
        />
      </Row>
      <Row width={{ minimum: { chars: 18 }, maximum: { chars: 30 } }} height="fill">
        <Navigation section={section} sections={sections} onSelect={setSection} />
      </Row>
      {body}
    </Responsive>
  );
}

function sectionTitle(value: Section): string {
  return value.charAt(0).toUpperCase() + value.slice(1);
}

function useResource<T>(loader: () => Promise<T[]>, initial?: T[]): Resource<T> {
  const [data, setData] = useState<T[] | undefined>(initial);
  const [loading, setLoading] = useState(initial === undefined);
  const [error, setError] = useState<unknown>(null);
  const revision = useRef(0);
  const reload = useCallback(async () => {
    const requested = ++revision.current;
    setLoading(true);
    try {
      const value = await loader();
      if (requested !== revision.current) return;
      setData(value);
      setError(null);
    } catch (cause) {
      if (requested === revision.current) setError(cause);
    } finally {
      if (requested === revision.current) setLoading(false);
    }
  }, [loader]);
  const replace = useCallback((value: T[]) => {
    revision.current += 1;
    setData(value);
    setError(null);
    setLoading(false);
  }, []);
  useEffect(() => {
    if (initial === undefined) void reload();
    return () => {
      revision.current += 1;
    };
  }, [initial, reload]);
  return { data, loading, error, reload, replace };
}
