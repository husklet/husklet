import React from 'react';
import {
  Button,
  Card,
  CardActionArea,
  CardContent,
  Column,
  Heading,
  Icon,
  NavigationMenu,
  NavigationMenuItem,
  Row,
  Scroll,
  Spinner,
  Spacer,
  Text,
  type ContainerSummary,
  type ExecutionSummary,
  type ExtensionSummary,
  type ImageSummary,
  type NetworkSummary,
  type TabSummary,
  type VolumeSummary,
} from '@husklet/react';
import { boundedMessage } from './model.js';

export const SECTIONS = [
  'overview',
  'workspace',
  'extensions',
  'containers',
  'processes',
  'executions',
  'images',
  'volumes',
  'networks',
  'terminals',
] as const;
export type Section = (typeof SECTIONS)[number];

export type Resource<T> = {
  data: T[] | undefined;
  loading: boolean;
  error: unknown;
  reload: () => Promise<void>;
  replace: (value: T[]) => void;
};

export function Navigation({
  section,
  onSelect,
}: {
  section: Section;
  onSelect: (section: Section) => void;
}) {
  const groups: { label: string; sections: Section[] }[] = [
    { label: 'Workspace', sections: ['overview', 'workspace', 'extensions'] },
    { label: 'Runtime', sections: ['containers', 'processes', 'executions'] },
    { label: 'Resources', sections: ['images', 'volumes', 'networks'] },
    { label: 'Interface', sections: ['terminals'] },
  ];
  return (
    <Column grow={false} width="fill" height="fill" pad={1} gap={1}>
      <Scroll grow height="fill">
        <Column gap={1}>
          {groups.map((group) => (
            <Column key={group.label} gap={0}>
              <Text label={group.label.toUpperCase()} color="text-dim" />
              <NavigationMenu gap={0}>
                {group.sections.map((name) => (
                  <NavigationMenuItem
                    key={name}
                    label={title(name)}
                    icon={navigationIcon(name)}
                    selected={section === name}
                    variant={section === name ? 'filled' : 'ghost'}
                    tone={section === name ? 'accent' : 'neutral'}
                    tooltip={`Open ${title(name)}`}
                    onInvoke={() => onSelect(name)}
                  />
                ))}
              </NavigationMenu>
            </Column>
          ))}
        </Column>
      </Scroll>
    </Column>
  );
}

function navigationIcon(section: Section): string {
  if (section === 'overview') return 'view-grid-symbolic';
  if (section === 'workspace') return 'document-open-symbolic';
  if (section === 'extensions') return 'list-add-symbolic';
  if (section === 'containers') return 'view-list-symbolic';
  if (section === 'processes') return 'edit-find-symbolic';
  if (section === 'executions') return 'system-run-symbolic';
  if (section === 'images') return 'drive-harddisk-symbolic';
  if (section === 'volumes') return 'folder-symbolic';
  if (section === 'networks') return 'network-workgroup-symbolic';
  return 'view-more-symbolic';
}

export function Overview({
  containers,
  executions,
  images,
  volumes,
  networks,
  terminals,
  extensions,
  onOpen,
}: {
  containers: Resource<ContainerSummary>;
  executions: Resource<ExecutionSummary>;
  images: Resource<ImageSummary>;
  volumes: Resource<VolumeSummary>;
  networks: Resource<NetworkSummary>;
  terminals: Resource<TabSummary>;
  extensions: Resource<ExtensionSummary>;
  onOpen: (section: Section) => void;
}) {
  const resources = [containers, executions, images, volumes, networks, terminals, extensions];
  const refreshing = resources.some((resource) => resource.loading);
  const refreshAll = async () => {
    await Promise.all(resources.map((resource) => resource.reload()));
  };
  const containersSummary = resourceSummary(
    containers,
    (records) => `${records.filter((item) => item.state === 'running').length} running`,
  );
  const executionsSummary = resourceSummary(
    executions,
    (records) => `${records.filter((item) => item.running).length} running`,
  );
  const imagesSummary = resourceSummary(images, () => 'Available locally');
  const volumesSummary = resourceSummary(volumes, () => 'Durable local storage');
  const networksSummary = resourceSummary(networks, () => 'Workspace-local connectivity');
  const terminalsSummary = resourceSummary(
    terminals,
    (records) => `${records.filter((tab) => tab.pinned).length} pinned`,
  );
  const extensionsSummary = resourceSummary(extensions, (records) => {
    const faults = records.filter((extension) => extension.status.startsWith('fault:')).length;
    return faults ? `${faults} need attention` : 'No reported faults';
  });
  const runningContainers = containers.data?.filter((item) => item.state === 'running').length ?? 0;
  return (
    <Scroll grow height="fill">
      <Column width="fill" pad={2} gap={2}>
        <Heading label="Workspace overview" scale="title" />
        <Text label="Current inventory and reported runtime attention." color="text-dim" wrap />
        <Row gap={1} align="center">
          {refreshing ? <Spinner /> : null}
          <Button
            label={refreshing ? 'Refreshing…' : 'Refresh all'}
            enabled={!refreshing}
            onInvoke={refreshAll}
          />
        </Row>
        <Row width="fill" gap={2} wrap>
          <Summary title="Containers" {...containersSummary} onOpen={() => onOpen('containers')} />
          <Summary
            title="Processes"
            value={
              containers.loading
                ? '…'
                : containers.error
                  ? 'Unavailable'
                  : String(runningContainers)
            }
            detail="running containers available to snapshot"
            onOpen={() => onOpen('processes')}
          />
          <Summary title="Executions" {...executionsSummary} onOpen={() => onOpen('executions')} />
          <Summary title="Images" {...imagesSummary} onOpen={() => onOpen('images')} />
          <Summary title="Volumes" {...volumesSummary} onOpen={() => onOpen('volumes')} />
          <Summary title="Networks" {...networksSummary} onOpen={() => onOpen('networks')} />
          <Summary title="Terminal tabs" {...terminalsSummary} onOpen={() => onOpen('terminals')} />
          <Summary title="Extensions" {...extensionsSummary} onOpen={() => onOpen('extensions')} />
        </Row>
        <ErrorText
          error={
            containers.error ??
            executions.error ??
            images.error ??
            volumes.error ??
            networks.error ??
            terminals.error
          }
        />
      </Column>
    </Scroll>
  );
}

function resourceSummary<T>(
  resource: Pick<Resource<T>, 'data' | 'loading' | 'error'>,
  readyDetail: (records: T[]) => string,
) {
  if (resource.loading) return { value: '…', detail: 'Reading inventory…' };
  if (resource.error) return { value: 'Unavailable', detail: 'Refresh failed' };
  const records = resource.data ?? [];
  return { value: String(records.length), detail: readyDetail(records) };
}

function Summary({
  title: label,
  value,
  detail,
  onOpen,
}: {
  title: string;
  value: string;
  detail: string;
  onOpen: () => void;
}) {
  return (
    <Card grow={false} width={{ minimum: { chars: 18 } }} variant="outline">
      <CardActionArea tooltip={`Open ${label}`} onInvoke={onOpen}>
        <CardContent gap={1} pad={1}>
          <Row gap={1} align="center" width="fill">
            <Text label={label} color="text-dim" />
            <Spacer />
            <Icon icon="go-next-symbolic" tooltip={`Open ${label}`} />
          </Row>
          <Row gap={1} align="center" wrap>
            <Heading label={value} scale="title" />
            <Text label={detail} color="text-dim" wrap />
          </Row>
        </CardContent>
      </CardActionArea>
    </Card>
  );
}

function ErrorText({ error }: { error: unknown }) {
  return error ? <Text label={boundedMessage(error)} color="danger" wrap /> : null;
}

function title(value: string): string {
  return value.charAt(0).toUpperCase() + value.slice(1);
}
