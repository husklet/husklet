import React from 'react';
import {
  Button, Card, CardActions, CardContent, CardHeader, Column, Heading, List, ListItemButton,
  Row, Scroll, Text,
  type ContainerSummary, type ImageSummary, type NetworkSummary, type TabSummary, type VolumeSummary,
} from '@husklet/react';
import { boundedMessage } from './model.js';

export const SECTIONS = ['overview', 'containers', 'processes', 'executions', 'images', 'volumes', 'networks', 'terminals'] as const;
export type Section = typeof SECTIONS[number];

export type Resource<T> = {
  data: T[] | undefined;
  loading: boolean;
  error: unknown;
  reload: () => Promise<void>;
  replace: (value: T[]) => void;
};

export function Navigation({ section, onSelect }: { section: Section; onSelect: (section: Section) => void }) {
  return <Column width={{ chars: 22 }} height="fill" pad={2} gap={1}>
    <Heading label="Top" scale="title" />
    <Text label="Runtime resources" color="text-dim" />
    <List grow>
      {SECTIONS.map((name) => <ListItemButton
        key={name}
        label={title(name)}
        variant={section === name ? 'filled' : 'ghost'}
        onInvoke={() => onSelect(name)} />)}
    </List>
  </Column>;
}

export function Overview({ containers, images, volumes, networks, terminals = { data: [], loading: false, error: null }, onOpen }: {
  containers: Resource<ContainerSummary>; images: Resource<ImageSummary>; volumes: Resource<VolumeSummary>;
  networks: Resource<NetworkSummary>; terminals?: Pick<Resource<TabSummary>, 'data' | 'loading' | 'error'>;
  onOpen: (section: Section) => void;
}) {
  const containersSummary = resourceSummary(containers, (records) => `${records.filter((item) => item.state === 'running').length} running`);
  const imagesSummary = resourceSummary(images, () => 'Available locally');
  const volumesSummary = resourceSummary(volumes, () => 'Durable local storage');
  const networksSummary = resourceSummary(networks, () => 'Workspace-local connectivity');
  const terminalsSummary = resourceSummary(terminals, (records) => `${records.filter((tab) => tab.pinned).length} pinned`);
  return <Scroll grow height="fill"><Column pad={4} gap={3}>
    <Heading label="Resource overview" scale="title" />
    <Text label="Inspect and operate everything running in this workspace." color="text-dim" />
    <Row gap={2} wrap>
      <Summary title="Containers" {...containersSummary} onOpen={() => onOpen('containers')} />
      <Summary title="Images" {...imagesSummary} onOpen={() => onOpen('images')} />
      <Summary title="Volumes" {...volumesSummary} onOpen={() => onOpen('volumes')} />
      <Summary title="Networks" {...networksSummary} onOpen={() => onOpen('networks')} />
      <Summary title="Terminal tabs" {...terminalsSummary} onOpen={() => onOpen('terminals')} />
    </Row>
    <ErrorText error={containers.error ?? images.error ?? volumes.error ?? networks.error ?? terminals.error} />
  </Column></Scroll>;
}

function resourceSummary<T>(resource: Pick<Resource<T>, 'data' | 'loading' | 'error'>, readyDetail: (records: T[]) => string) {
  if (resource.loading) return { value: '…', detail: 'Reading inventory…' };
  if (resource.error) return { value: 'Unavailable', detail: 'Refresh failed' };
  const records = resource.data ?? [];
  return { value: String(records.length), detail: readyDetail(records) };
}

function Summary({ title: label, value, detail, onOpen }: { title: string; value: string; detail: string; onOpen: () => void }) {
  return <Card width={{ minimum: { chars: 18 } }} variant="outline">
    <CardHeader label={label} />
    <CardContent gap={1}><Heading label={value} scale="title" /><Text label={detail} color="text-dim" /></CardContent>
    <CardActions><Button label="Open" variant="ghost" onInvoke={onOpen} /></CardActions>
  </Card>;
}

function ErrorText({ error }: { error: unknown }) {
  return error ? <Text label={boundedMessage(error)} color="danger" wrap /> : null;
}

function title(value: string): string { return value.charAt(0).toUpperCase() + value.slice(1); }
