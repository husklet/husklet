import React from 'react';
import {
  Row, Separator,
  type ContainerSummary, type ExecutionSummary, type HostEvent, type ImageSummary, type NetworkSummary,
  type TabSummary, type VolumeSummary, type WorkspaceApi,
} from '@husklet/react';
import { ContainerDetailsSource, ExecutionDetailsSource, ImageDetailsSource, NetworkDetailsSource, VolumeDetailsSource } from './model.js';
import { Navigation, Overview, SECTIONS, type Resource, type Section } from './overview.js';
import { Terminals } from './terminals.js';
import { Processes } from './processes.js';
import { Executions } from './executions.js';
import { Images } from './images.js';
import { Volumes } from './volumes.js';
import { Networks } from './networks.js';
import { Containers } from './containers.js';

export { Overview, SECTIONS } from './overview.js';
export { Terminals } from './terminals.js';
export { Processes } from './processes.js';
export { Executions } from './executions.js';
export { Images } from './images.js';
export { Volumes } from './volumes.js';
export { Networks } from './networks.js';
export { ContainerRename } from './container-rename.js';
export { ContainerCreate } from './container-create.js';
export { ContainerDetail } from './container-detail.js';
export { Containers } from './containers.js';

const { useCallback, useEffect, useRef, useState } = React;
type Selections = { subscribe(listener: (event: HostEvent) => void): (() => void) | undefined };
type TopProps = {
  api: WorkspaceApi;
  selections?: Selections;
  containerDetails?: ContainerDetailsSource;
  executionDetails?: ExecutionDetailsSource;
  imageDetails?: ImageDetailsSource;
  networkDetails?: NetworkDetailsSource;
  volumeDetails?: VolumeDetailsSource;
  initial?: Partial<{
    containers: ContainerSummary[]; executions: ExecutionSummary[]; images: ImageSummary[];
    volumes: VolumeSummary[]; networks: NetworkSummary[]; terminals: TabSummary[];
  }>;
};
export function Top({ api, selections, containerDetails, executionDetails, imageDetails, networkDetails, volumeDetails, initial = {} }: TopProps) {
  const [section, setSection] = useState<Section>('overview');
  const [requestedExecution, setRequestedExecution] = useState('');
  const containers = useResource(api.containers.list, initial.containers);
  const images = useResource(api.images.list, initial.images);
  const volumes = useResource(api.volumes.list, initial.volumes);
  const networks = useResource(api.networks.list, initial.networks);
  const terminals = useResource(api.terminal?.tabs ?? (async () => []), initial.terminals);
  const [executionsTruncated, setExecutionsTruncated] = useState(false);
  const listExecutions = useCallback(async () => {
    const listing = await api.containers.executions();
    setExecutionsTruncated(listing.truncated);
    return listing.executions;
  }, [api]);
  const executions = useResource(listExecutions, initial.executions);
  useEffect(() => {
    if (section !== 'executions' || typeof api.watchExecutions !== 'function') return undefined;
    let disposed = false;
    let stop: (() => void) | null = null;
    void api.watchExecutions((listing) => {
      if (disposed) return;
      setExecutionsTruncated(listing.truncated);
      executions.replace(listing.executions);
    }).then((dispose) => {
      if (disposed) void dispose();
      else stop = dispose;
    }).catch(() => { /* Explicit Refresh remains available when observation is unsupported. */ });
    return () => {
      disposed = true;
      if (stop) void stop();
    };
  }, [api, section, executions.replace]);
  useEffect(() => selections?.subscribe((event) => {
    if ('pane_provider' in event && SECTIONS.includes(event.pane_provider as Section)) setSection(event.pane_provider as Section);
    if ('snapshot' in event && event.snapshot === 'containers') void containers.reload();
    if ('snapshot' in event && event.snapshot === 'images') void images.reload();
    if ('snapshot' in event && event.snapshot === 'volumes') void volumes.reload();
    if ('snapshot' in event && event.snapshot === 'networks') void networks.reload();
    if ('snapshot' in event && event.snapshot === 'terminal') void terminals.reload();
  }), [selections, containers.reload, images.reload, volumes.reload, networks.reload, terminals.reload]);
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
  const body = section === 'overview'
    ? <Overview
    containers={containers}
    executions={executions}
    images={images}
    volumes={volumes}
    networks={networks}
    terminals={terminals}
    onOpen={setSection} />
    : section === 'containers'
      ? <Containers
    api={api}
    resource={containers}
    containerDetails={containerDetails}
    onOpenExecution={async (id: string) => {
          setRequestedExecution(id);
          await executions.reload();
          setSection('executions');
        }} />
      : section === 'processes'
        ? <Processes api={api} resource={containers} />
        : section === 'executions'
          ? <Executions
    api={api}
    resource={executions}
    executionDetails={executionDetails}
    truncated={executionsTruncated}
    requestedExecution={requestedExecution} />
        : section === 'images'
          ? <Images api={api} resource={images} imageDetails={imageDetails} />
          : section === 'volumes'
            ? <Volumes api={api} resource={volumes} volumeDetails={volumeDetails} />
            : section === 'networks'
              ? <Networks api={api} resource={networks} networkDetails={networkDetails} />
              : <Terminals api={api} resource={terminals} />;
  return (
    <Row grow={true} gap={0}>
      <Navigation section={section} onSelect={setSection} />
      <Separator orientation={'vertical'} />
      {body}
    </Row>
  );
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
      setData(value); setError(null);
    } catch (cause) {
      if (requested === revision.current) setError(cause);
    } finally {
      if (requested === revision.current) setLoading(false);
    }
  }, [loader]);
  const replace = useCallback((value: T[]) => { revision.current += 1; setData(value); setError(null); setLoading(false); }, []);
  useEffect(() => {
    if (initial === undefined) void reload();
    return () => { revision.current += 1; };
  }, [initial, reload]);
  return { data, loading, error, reload, replace };
}
