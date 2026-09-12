// The playground: pick a component on the left, see it in the middle, change
// its properties on the right.
//
// Every list here is the catalogue's; nothing about the component library is
// spelled out in this file.

import React from 'react';
import type { Key, ReactNode } from 'react';
import {
  Button,
  Column,
  Entry,
  Heading,
  InlineMessage,
  ListItemButton,
  ListSubheader,
  NumberEntry,
  Responsive,
  Row,
  Scroll,
  Section,
  Select,
  Switch,
  Text,
  components,
  type Report,
} from '@husklet/react';

import { component, grouped, notes, type Family, type Tag } from './catalogue.js';
import { OPENING, defaults, spaced, type StoryChild, type StoryDefaults } from './defaults.js';
import { amountOf, lengthValue, modeOf, rows, type ControlRow } from './editors.js';
import {
  ApiReference,
  ComponentDocument,
  DocumentationSection,
  SpecimenGrid,
} from './component-document.js';
import { LargeDataTableStory, LargeRecordSource } from './large-table.js';
import { ACQUISITION_STORY, AcquisitionProgressStory } from './acquisition.js';
import { FORM_STORY, ValidatedSettingsFormStory } from './form.js';
import { KEYBOARD_STORY, KeyboardAccessibilityStory } from './keyboard-accessibility.js';
import { STREAMING_LOG_STORY, StreamingLogStory } from './streaming-log.js';
import { EVENT_STREAM_STORY, EventStreamStory, TimelineSource } from './event-stream.js';
import { KEY_VALUE_STORY, KeyValueInspectorStory, KeyValueSource } from './key-value-inspector.js';
import { MARKDOWN_STORY, MarkdownReviewStory } from './markdown-review.js';
import { NAVIGATION_STORY, NavigationDialogsStory } from './navigation-dialogs.js';
import { DIFF_STORY, DiffReviewStory } from './diff-review.js';
import { JSON_STORY, JsonResponseStory } from './json-response.js';
import { STACK_STORY, StackTraceStory } from './stack-trace.js';
import { BINARY_STORY, BinaryInspectionStory } from './binary-inspection.js';
import { METRICS_STORY, ResourceMetricsStory } from './resource-metrics.js';
import { FILE_BROWSER_STORY, FileBrowserStory } from './file-browser.js';
import { PROFILE_STORY, ProfileInspectionStory } from './profile-inspection.js';
import { MEMORY_STORY, MemoryInspectionStory } from './memory-inspection.js';
import { DISASSEMBLY_STORY, DisassemblyInspectionStory } from './disassembly-inspection.js';
import { TIMELINE_VIEW_STORY, TimelineInspectionStory } from './timeline-inspection.js';
import { TEST_REPORT_STORY, TestReportStory } from './test-report.js';
import { COVERAGE_STORY, CoverageInspectionStory } from './coverage-inspection.js';
import { NETWORK_WATERFALL_STORY, NetworkWaterfallStory } from './network-waterfall.js';
import { DEPENDENCY_GRAPH_STORY, DependencyGraphStory } from './dependency-graph.js';
import { QUERY_PLAN_STORY, QueryPlanStory } from './query-plan.js';
import { CONTAINER_OPERATIONS_STORY, ContainerOperationsStory } from './container-operations.js';
import { WORKSPACE_LAYOUT_STORY, WorkspaceLayoutStory } from './workspace-layout.js';
import { EXTENSION_LIFECYCLE_STORY, ExtensionLifecycleStory } from './extension-lifecycle.js';
import { WORKSPACE_FILE_EDIT_STORY, WorkspaceFileEditStory } from './workspace-file-edit.js';
import { IMAGE_PULL_STORY, ImagePullStory } from './image-pull.js';
import { DRAG_REORDER_STORY, DragReorderStory } from './drag-reorder.js';
import { componentPage, renderComponentPage } from './component-pages.js';

const { useMemo, useRef, useState } = React;

const INTERACTION_HISTORY = 5;
export const SEARCH_RESULT_LIMIT = 24;
export const FLOW_STORIES = Object.freeze([
  DRAG_REORDER_STORY,
  IMAGE_PULL_STORY,
  WORKSPACE_FILE_EDIT_STORY,
  EXTENSION_LIFECYCLE_STORY,
  WORKSPACE_LAYOUT_STORY,
  CONTAINER_OPERATIONS_STORY,
  QUERY_PLAN_STORY,
  DEPENDENCY_GRAPH_STORY,
  NETWORK_WATERFALL_STORY,
  COVERAGE_STORY,
  TEST_REPORT_STORY,
  TIMELINE_VIEW_STORY,
  DISASSEMBLY_STORY,
  MEMORY_STORY,
  PROFILE_STORY,
  FILE_BROWSER_STORY,
  METRICS_STORY,
  BINARY_STORY,
  ACQUISITION_STORY,
  KEYBOARD_STORY,
  STREAMING_LOG_STORY,
  EVENT_STREAM_STORY,
  KEY_VALUE_STORY,
  JSON_STORY,
  STACK_STORY,
  MARKDOWN_STORY,
  FORM_STORY,
  DIFF_STORY,
  NAVIGATION_STORY,
]);

type StoryFamily = Family & { tags: Tag[] };
type Change = (name: string, value: unknown) => void;
type PlaygroundProps = {
  largeSource?: LargeRecordSource;
  timelineSource?: TimelineSource;
  keyValueSource?: KeyValueSource;
  fileSource?: unknown;
  initialStory?: string;
};
type SearchResult = {
  kind: NavigationMode;
  name: string;
  detail: string;
  family: string | null;
};
type NavigationMode = 'component' | 'pattern';
type Interaction = { sequence: number; trigger: string; detail: string };

/** The whole playground. */
export function Playground({
  largeSource,
  timelineSource,
  keyValueSource,
  fileSource,
  initialStory = OPENING,
}: PlaygroundProps = {}) {
  const families = useMemo(grouped, []);
  const [selected, setSelected] = useState(initialStory);
  const hasComponentPage = componentPage(selected) !== undefined;
  const mode = modeFor(selected);
  const [activeFamily, setActiveFamily] = useState(
    () =>
      families.find((family) => family.tags.some((tag) => tag.name === initialStory))?.name ??
      families.find((family) => family.tags.some((tag) => tag.name === OPENING))?.name ??
      families[0]?.name,
  );
  const flow = FLOW_STORIES.includes(selected);
  const opened = flow || hasComponentPage ? null : defaults(selected);
  const contract = flow || hasComponentPage ? null : component(selected);
  const allStories = [
    ...FLOW_STORIES,
    ...families.flatMap((family) => family.tags.map((tag) => tag.name)),
  ];
  const selectStory = (name: string) => {
    const nextMode = modeFor(name);
    if (nextMode === 'component') {
      const nextFamily = families.find((family) => family.tags.some((tag) => tag.name === name));
      if (nextFamily) setActiveFamily(nextFamily.name);
    }
    setSelected(name);
  };

  return (
    <Responsive breakpoint={1024} position={240} grow>
      <Row width="fill" pad={2} gap={2} align="center">
        <Text label="Page" color="text-dim" />
        <Select
          value={selected}
          width="fill"
          choices={allStories.map((name) => ({
            value: name,
            label: `${modeLabel(modeFor(name))} · ${spaced(name)}`,
          }))}
          onChange={(report) => selectStory(String(report.value))}
        />
      </Row>
      <Sidebar
        key={'sidebar'}
        families={families}
        selected={selected}
        mode={mode}
        activeFamily={activeFamily}
        onMode={(nextMode) => selectStory(nextMode === 'component' ? OPENING : FLOW_STORIES[0])}
        onFamily={setActiveFamily}
        onSelect={selectStory}
      />
      <Scroll grow width="fill" height="fill">
        {hasComponentPage ? (
          renderComponentPage(selected)
        ) : (
          <Preview
            key={`preview-${selected}`}
            name={selected}
            opened={opened}
            largeSource={largeSource}
            timelineSource={timelineSource}
            keyValueSource={keyValueSource}
            fileSource={fileSource}
            triggers={contract ? [...contract.triggers] : []}
          />
        )}
      </Scroll>
    </Responsive>
  );
}

/** Every component, under the family it belongs to. */
export function Sidebar({
  families,
  selected,
  mode,
  activeFamily,
  onMode,
  onFamily,
  onSelect,
}: {
  families: StoryFamily[];
  selected: string;
  mode: NavigationMode;
  activeFamily?: string;
  onMode: (mode: NavigationMode) => void;
  onFamily: (family: string) => void;
  onSelect: (story: string) => void;
}) {
  const [search, setSearch] = useState('');
  const family = families.find((candidate) => candidate.name === activeFamily) ?? families[0];
  const query = search.trim().toLocaleLowerCase();
  const results = searchResults(families, query);
  return (
    <Scroll width={{ chars: 26 }} height={'fill'}>
      <Column pad={1} gap={1}>
        <ListSubheader key={'browse'} label={'Library'} />
        <Select
          key={'mode'}
          value={mode}
          choices={[
            { value: 'component', label: 'Components' },
            { value: 'pattern', label: 'Product patterns' },
          ]}
          onChange={(event) => onMode(event.value as NavigationMode)}
        />
        <Entry
          key={'search'}
          width={'fill'}
          value={search}
          placeholder={'Search all pages'}
          tooltip={'search every component and product pattern'}
          onChange={(event) => setSearch(String(event.value ?? '').slice(0, 80))}
        />
        {query.length > 0
          ? [
              <ListSubheader
                key={'results'}
                label={'Search results'}
                value={String(results.length)}
              />,
              ...results.map((result) => (
                <ListItemButton
                  key={`${result.kind}:${result.name}`}
                  label={`${modeLabel(result.kind)} · ${result.name}`}
                  tooltip={result.detail}
                  variant={selected === result.name ? 'filled' : 'ghost'}
                  onInvoke={() => onSelect(result.name)}
                />
              )),
              ...(results.length === 0
                ? [
                    <Text
                      key={'none'}
                      label={'No flows or components match this search.'}
                      color={'text-dim'}
                      wrap={true}
                    />,
                  ]
                : []),
            ]
          : [
              ...(mode === 'component'
                ? [
                    <Select
                      key={'family'}
                      width={'fill'}
                      tooltip={'choose one bounded catalogue family'}
                      value={family.name}
                      choices={families.map((candidate) => ({
                        value: candidate.name,
                        label: candidate.label,
                      }))}
                      onChange={(event) => onFamily(String(event.value))}
                    />,
                    <ListSubheader key={family.name} label={family.label} tooltip={family.note} />,
                    ...family.tags.map((tag) => (
                      <ListItemButton
                        key={tag.name}
                        label={tag.name}
                        variant={tag.name === selected ? 'filled' : 'ghost'}
                        onInvoke={() => onSelect(tag.name)}
                      />
                    )),
                  ]
                : [
                    <ListSubheader
                      key={'patterns'}
                      label={'Product patterns'}
                      tooltip={'complete product states composed from the components'}
                    />,
                    ...FLOW_STORIES.map((story) => (
                      <ListItemButton
                        key={story}
                        label={story}
                        variant={selected === story ? 'filled' : 'ghost'}
                        onInvoke={() => onSelect(story)}
                      />
                    )),
                  ]),
            ]}
      </Column>
    </Scroll>
  );
}

/** A bounded global navigation projection; no query materializes the catalogue. */
export function searchResults(families: StoryFamily[], query: unknown): SearchResult[] {
  const normalized = String(query ?? '')
    .trim()
    .toLocaleLowerCase();
  if (normalized.length === 0) return [];
  const flows = FLOW_STORIES.filter((name) => name.toLocaleLowerCase().includes(normalized)).map(
    (name): SearchResult => ({ kind: 'pattern', name, detail: 'Product pattern', family: null }),
  );
  const components = families.flatMap((family) =>
    family.tags
      .filter((tag) => tag.name.toLocaleLowerCase().includes(normalized))
      .map((tag): SearchResult => ({
        kind: 'component',
        name: tag.name,
        detail: family.label,
        family: family.name,
      })),
  );
  return [...flows, ...components].slice(0, SEARCH_RESULT_LIMIT);
}

function modeFor(name: string): NavigationMode {
  return FLOW_STORIES.includes(name) ? 'pattern' : 'component';
}

function modeLabel(mode: NavigationMode): string {
  return mode === 'component' ? 'Component' : 'Pattern';
}

/** The selected component, alive, with the properties currently set on it. */
export function Preview({
  name,
  opened,
  largeSource,
  timelineSource,
  keyValueSource,
  fileSource,
  triggers = [],
}: PlaygroundProps & {
  name: string;
  opened: StoryDefaults | null;
  triggers?: string[];
}) {
  const [interactions, setInteractions] = useState<Interaction[]>([]);
  const sequence = useRef(0);
  const handlers = interactionProps(triggers, (trigger, event) => {
    const interaction = { sequence: ++sequence.current, trigger, detail: interactionDetail(event) };
    setInteractions((current) => [...current, interaction].slice(-INTERACTION_HISTORY));
  });
  if (name === 'DataTable' && largeSource) {
    return <LargeDataTableStory source={largeSource} />;
  }
  // The final branch below is reachable only for catalogue components; flow
  // stories are exhausted by the branches above and intentionally have no defaults.
  const instance = opened as StoryDefaults;
  const flow = FLOW_STORIES.includes(name);
  if (!flow) {
    return (
      <CatalogueDocument
        name={name}
        instance={instance}
        handlers={handlers}
        triggers={triggers}
        interactions={interactions}
        clearInteractions={() => setInteractions([])}
      />
    );
  }
  return (
    <Column grow={true} gap={2} pad={4}>
      {flow ? null : <Heading key={'title'} label={spaced(name)} scale={'title'} wrap={true} />}
      <Section key={'stage'} pad={flow ? 0 : 4} grow={true}>
        {name === DRAG_REORDER_STORY ? (
          <DragReorderStory />
        ) : name === QUERY_PLAN_STORY ? (
          <QueryPlanStory />
        ) : name === IMAGE_PULL_STORY ? (
          <ImagePullStory />
        ) : name === WORKSPACE_FILE_EDIT_STORY ? (
          <WorkspaceFileEditStory />
        ) : name === EXTENSION_LIFECYCLE_STORY ? (
          <ExtensionLifecycleStory />
        ) : name === WORKSPACE_LAYOUT_STORY ? (
          <WorkspaceLayoutStory />
        ) : name === CONTAINER_OPERATIONS_STORY ? (
          <ContainerOperationsStory />
        ) : name === DEPENDENCY_GRAPH_STORY ? (
          <DependencyGraphStory />
        ) : name === NETWORK_WATERFALL_STORY ? (
          <NetworkWaterfallStory />
        ) : name === COVERAGE_STORY ? (
          <CoverageInspectionStory />
        ) : name === TEST_REPORT_STORY ? (
          <TestReportStory />
        ) : name === TIMELINE_VIEW_STORY ? (
          <TimelineInspectionStory />
        ) : name === DISASSEMBLY_STORY ? (
          <DisassemblyInspectionStory />
        ) : name === MEMORY_STORY ? (
          <MemoryInspectionStory />
        ) : name === PROFILE_STORY ? (
          <ProfileInspectionStory />
        ) : name === FILE_BROWSER_STORY && fileSource ? (
          <FileBrowserStory />
        ) : name === METRICS_STORY ? (
          <ResourceMetricsStory />
        ) : name === BINARY_STORY ? (
          <BinaryInspectionStory />
        ) : name === ACQUISITION_STORY ? (
          <AcquisitionProgressStory />
        ) : name === DIFF_STORY ? (
          <DiffReviewStory />
        ) : name === FORM_STORY ? (
          <ValidatedSettingsFormStory />
        ) : name === KEYBOARD_STORY ? (
          <KeyboardAccessibilityStory />
        ) : name === STREAMING_LOG_STORY ? (
          <StreamingLogStory />
        ) : name === EVENT_STREAM_STORY && timelineSource ? (
          <EventStreamStory source={timelineSource} />
        ) : name === KEY_VALUE_STORY && keyValueSource ? (
          <KeyValueInspectorStory source={keyValueSource} />
        ) : name === MARKDOWN_STORY ? (
          <MarkdownReviewStory />
        ) : name === JSON_STORY ? (
          <JsonResponseStory />
        ) : name === STACK_STORY ? (
          <StackTraceStory />
        ) : name === NAVIGATION_STORY ? (
          <NavigationDialogsStory />
        ) : (
          nativeComponent(
            name,
            { ...present(instance.props), ...handlers },
            instance.children.map(child),
          )
        )}
      </Section>
      {triggers.length === 0
        ? []
        : [
            <Column key={'interaction-console'} gap={1}>
              <Row key={'heading'} align={'center'} gap={1}>
                <Text key={'title'} label={'Recent interactions'} color={'text-dim'} grow={true} />
                {interactions.length === 0
                  ? []
                  : [
                      <Button
                        key={'clear'}
                        label={'Clear'}
                        variant={'ghost'}
                        onInvoke={() => setInteractions([])}
                      />,
                    ]}
              </Row>
              {interactions.length === 0
                ? [
                    <InlineMessage
                      key={'empty'}
                      label={`Interact with the preview to inspect ${triggers.map((trigger) => `on${trigger}`).join(', ')}.`}
                      tone={'neutral'}
                    />,
                  ]
                : interactions.map((interaction) => (
                    <InlineMessage
                      key={interaction.sequence}
                      label={`#${interaction.sequence} ${interaction.trigger} received${interaction.detail ? ` · ${interaction.detail}` : ''}`}
                      tone={'positive'}
                    />
                  ))}
            </Column>,
          ]}
    </Column>
  );
}

/**
 * The generated reference page every catalogue component receives. Bespoke
 * pages replace this baseline only when they can teach more, never with less.
 */
export function CatalogueDocument({
  name,
  instance,
  handlers,
  triggers,
  interactions,
  clearInteractions,
}: {
  name: string;
  instance: StoryDefaults;
  handlers: Record<string, (event: Report) => void>;
  triggers: string[];
  interactions: Interaction[];
  clearInteractions: () => void;
}) {
  const contract = component(name);
  const family = grouped().find((candidate) => candidate.name === contract.family);
  const summary =
    family?.note ?? `${spaced(name)} belongs to the ${family?.label ?? contract.family} family.`;
  const specimen = nativeComponent(
    name,
    {
      ...present(instance.props),
      ...handlers,
      width: 'content',
      height: 'content',
      grow: false,
    },
    instance.children.map(child),
  );
  return (
    <ComponentDocument name={spaced(name)} summary={summary}>
      <DocumentationSection title="Overview">
        <Section pad={3} width="fill">
          <Row width="fill" align="center" justify="start">
            {specimen}
          </Row>
        </Section>
      </DocumentationSection>
      <DocumentationSection title="API">
        <ApiReference example={exampleFor(name, instance.props)} rows={rows(name)} />
      </DocumentationSection>
      <SpecimenGrid>
        <DocumentationSection title="Usage">
          <Text
            label={`Use ${spaced(name)} for ${family?.label.toLocaleLowerCase() ?? contract.family} interfaces. Prefer its semantic properties over manual sizing or color overrides.`}
            color="text-dim"
            wrap
          />
        </DocumentationSection>
        <DocumentationSection title="Interactions">
          <Text
            label={
              triggers.length === 0
                ? 'Presentational component. It reports no direct user interaction.'
                : `Reports ${triggers.map((trigger) => `on${trigger}`).join(', ')}. Exercise the live specimen to inspect bounded event details.`
            }
            color="text-dim"
            wrap
          />
        </DocumentationSection>
      </SpecimenGrid>
      {triggers.length > 0 ? (
        <DocumentationSection title="Event inspector">
          <InteractionConsole
            interactions={interactions}
            triggers={triggers}
            onClear={clearInteractions}
          />
        </DocumentationSection>
      ) : null}
    </ComponentDocument>
  );
}

function InteractionConsole({
  interactions,
  triggers,
  onClear,
}: {
  interactions: Interaction[];
  triggers: string[];
  onClear: () => void;
}) {
  return (
    <Column gap={1}>
      <Row align="center" gap={1}>
        <Text label="Recent interactions" color="text-dim" grow />
        {interactions.length > 0 ? (
          <Button label="Clear" size="small" variant="ghost" onInvoke={onClear} />
        ) : null}
      </Row>
      {interactions.length === 0 ? (
        <InlineMessage
          label={`Interact with the specimen to inspect ${triggers.map((trigger) => `on${trigger}`).join(', ')}.`}
          tone="neutral"
        />
      ) : (
        interactions.map((interaction) => (
          <InlineMessage
            key={interaction.sequence}
            label={`#${interaction.sequence} ${interaction.trigger} received${interaction.detail ? ` · ${interaction.detail}` : ''}`}
            tone="positive"
          />
        ))
      )}
    </Column>
  );
}

/** A concise copyable starting point derived from the same defaults as the specimen. */
export function exampleFor(name: string, props: Record<string, unknown>): string {
  const written = Object.entries(props)
    .filter(([, value]) => ['string', 'number', 'boolean'].includes(typeof value))
    .slice(0, 3)
    .map(([key, value]) =>
      typeof value === 'string' ? `${key}=${JSON.stringify(value)}` : `${key}={${String(value)}}`,
    );
  return `<${name}${written.length > 0 ? ` ${written.join(' ')}` : ''} />`;
}

/** Real handlers for every interaction the selected component declares. */
export function interactionProps(
  triggers: string[],
  receive: (trigger: string, event: Report) => void,
): Record<string, (event: Report) => void> {
  return Object.fromEntries(
    triggers.map((trigger) => [`on${trigger}`, (event: Report) => receive(trigger, event)]),
  );
}

/** A short, finite payload description suitable for the visible event console. */
export function interactionDetail(event: unknown): string {
  if (event === null || typeof event !== 'object') return '';
  const report = event as Record<string, unknown>;
  const fields = ['value', 'rows', 'key', 'pressed', 'focused', 'phase', 'x', 'y', 'button'];
  const detail = fields
    .filter((field) => report[field] !== undefined)
    .map((field) => `${field}=${JSON.stringify(boundedValue(report[field]))}`)
    .join(' ');
  return detail.slice(0, 240);
}

/** Bound payload work as well as its visible result: events are extension-controlled input. */
function boundedValue(value: unknown, depth = 0): unknown {
  if (typeof value === 'string') return value.length > 80 ? `${value.slice(0, 79)}…` : value;
  if (value === null || typeof value !== 'object') return value;
  if (depth >= 2) return '…';
  if (Array.isArray(value)) {
    const shown = value.slice(0, 3).map((entry) => boundedValue(entry, depth + 1));
    return value.length > shown.length
      ? [...shown, `… ${value.length - shown.length} more`]
      : shown;
  }
  const shown: Record<string, unknown> = {};
  const record = value as Record<string, unknown>;
  let count = 0;
  for (const key in record) {
    if (!Object.hasOwn(record, key)) continue;
    count += 1;
    if (count <= 4) shown[key] = boundedValue(record[key], depth + 1);
    if (count === 5) {
      shown['…'] = 'more';
      break;
    }
  }
  return shown;
}

/** One default child, as an element. */
function child(descriptor: StoryChild, index: number): ReactNode {
  return nativeComponent(descriptor.tag, { key: `child-${index}`, ...descriptor.props });
}

function nativeComponent(
  name: string,
  supplied: Record<string, unknown>,
  children: ReactNode[] = [],
): ReactNode {
  const { key, ...props } = supplied;
  const Component = components[name];
  return React.createElement(Component, { key: key as Key | null | undefined, ...props }, children);
}

/** The props as the component takes them; an unset property is simply absent. */
export function present(props: Record<string, unknown>): Record<string, unknown> {
  return Object.fromEntries(Object.entries(props).filter(([, value]) => value !== undefined));
}

/** One row per property, grouped, with the control its editor hint asks for. */
export function Inspector({
  name,
  properties,
  triggers,
  props,
  onChange,
}: {
  name: string;
  properties: ControlRow[];
  triggers: string[];
  props: Record<string, unknown>;
  onChange: Change;
}) {
  type Group = { key: string; group: string; editable: boolean; rows: ControlRow[] };
  const groups: Group[] = [];
  let current: Group | null = null;
  for (const row of properties) {
    const key = `${row.editable ? 'set' : 'read'}:${row.group}`;
    if (current === null || current.key !== key) {
      current = { key, group: row.group, editable: row.editable, rows: [] };
      groups.push(current);
    }
    current.rows.push(row);
  }
  return (
    <Scroll width={{ chars: 32 }} height={'fill'}>
      <Column pad={3} gap={2}>
        <Heading key={'title'} label={`${name} properties`} scale={'caption'} wrap={true} />
        <Text key={'note'} label={notes.values} color={'text-dim'} wrap={true} />
        {groups.flatMap((group) => [
          <ListSubheader key={`group-${group.key}`} label={group.group} />,
          ...group.rows.map((row) => (
            <Field key={row.name} row={row} value={props[row.name]} onChange={onChange} />
          )),
        ])}
        {triggers.length === 0
          ? []
          : [
              <ListSubheader key={'interactions'} label={'interactions'} />,
              ...triggers.map((trigger) => (
                <Text key={`trigger-${trigger}`} label={`on${trigger}`} color={'text-dim'} />
              )),
            ]}
      </Column>
    </Scroll>
  );
}

/** One property, with the control that edits it. */
export function Field({
  row,
  value,
  onChange,
}: {
  row: ControlRow;
  value: unknown;
  onChange: Change;
}) {
  return (
    <Row gap={2} align={'center'}>
      <Text key={'name'} label={row.name} tooltip={row.note} width={{ chars: 12 }} />
      <React.Fragment key={'control'}>{controls(row, value, onChange)}</React.Fragment>
    </Row>
  );
}

/** The controls a property's editor hint asks for, already wired to `onChange`. */
function controls(row: ControlRow, value: unknown, onChange: Change): ReactNode[] {
  switch (row.editor) {
    case 'text':
      return [
        <Entry
          key={'value'}
          value={value === undefined ? '' : String(value)}
          placeholder={row.note}
          onChange={(event) => onChange(row.name, event.value)}
        />,
      ];
    case 'switch':
      return [
        <Switch
          key={'value'}
          checked={Boolean(value)}
          onToggle={(event) =>
            onChange(row.name, event.value === null ? !value : Boolean(event.value))
          }
        />,
      ];
    case 'enum':
      return [
        <Select
          key={'value'}
          choices={row.members ?? []}
          value={value === undefined ? '' : String(value)}
          onChange={(event) => onChange(row.name, event.value)}
        />,
      ];
    case 'number':
      return [
        <NumberEntry
          key={'value'}
          value={typeof value === 'number' ? value : 0}
          onChange={(event) => onChange(row.name, Number(event.value))}
        />,
      ];
    case 'length':
    case 'edges': {
      const mode = modeOf(value);
      const amount = amountOf(value);
      return [
        <Select
          key={'mode'}
          choices={row.modes ?? []}
          value={mode}
          onChange={(event) => onChange(row.name, lengthValue(event.value, amount))}
        />,
        ...(mode === 'step' || mode === 'chars'
          ? [
              <NumberEntry
                key={'amount'}
                value={amount}
                minimum={0}
                maximum={mode === 'step' ? row.maximum : 120}
                step={1}
                onChange={(event) => onChange(row.name, lengthValue(mode, Number(event.value)))}
              />,
            ]
          : []),
      ];
    }
    default:
      return [<Text key={'value'} label={`${row.editor}: set in code`} color={'text-faint'} />];
  }
}
