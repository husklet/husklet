// The playground: pick a component on the left, see it in the middle, change
// its properties on the right.
//
// Every list here is the catalogue's; nothing about the component library is
// spelled out in this file.

import React from 'react';
import {
  Button,
  Column,
  Entry,
  Heading,
  InlineMessage,
  List,
  ListItemButton,
  ListSubheader,
  NumberEntry,
  Row,
  Scroll,
  Section,
  Select,
  Separator,
  Switch,
  Text,
  components,
} from '@husklet/react';

import { component, grouped, notes } from './catalogue.js';
import { OPENING, defaults, spaced } from './defaults.js';
import { amountOf, lengthValue, modeOf, rows } from './editors.js';
import { LargeDataTableStory } from './large-table.js';
import { ACQUISITION_STORY, AcquisitionProgressStory } from './acquisition.js';
import { FORM_STORY, ValidatedSettingsFormStory } from './form.js';
import { KEYBOARD_STORY, KeyboardAccessibilityStory } from './keyboard-accessibility.js';
import { STREAMING_LOG_STORY, StreamingLogStory } from './streaming-log.js';
import { EVENT_STREAM_STORY, EventStreamStory } from './event-stream.js';
import { KEY_VALUE_STORY, KeyValueInspectorStory } from './key-value-inspector.js';
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
import { TERMINAL_TRANSCRIPT_STORY, TerminalTranscriptStory } from './terminal-transcript.js';
import { COMMAND_PALETTE_STORY, CommandPaletteStory } from './command-palette.js';
import { JSON_TREE_STORY, JsonTreeStory } from './json-tree.js';
import { CONFIRMATION_STORY, ConfirmationStory } from './confirmation.js';
import { CONTAINER_OPERATIONS_STORY, ContainerOperationsStory } from './container-operations.js';
import { WORKSPACE_LAYOUT_STORY, WorkspaceLayoutStory } from './workspace-layout.js';
import { EXTENSION_LIFECYCLE_STORY, ExtensionLifecycleStory } from './extension-lifecycle.js';
import { WORKSPACE_FILE_EDIT_STORY, WorkspaceFileEditStory } from './workspace-file-edit.js';
import { IMAGE_PULL_STORY, ImagePullStory } from './image-pull.js';
import { RESOURCE_STATE_STORY, ResourceStateStory } from './resource-state.js';

const { createElement: h, useMemo, useRef, useState } = React;

const INTERACTION_HISTORY = 5;
export const FLOW_STORIES = Object.freeze([
  RESOURCE_STATE_STORY, IMAGE_PULL_STORY, WORKSPACE_FILE_EDIT_STORY, EXTENSION_LIFECYCLE_STORY, WORKSPACE_LAYOUT_STORY, CONTAINER_OPERATIONS_STORY, CONFIRMATION_STORY, COMMAND_PALETTE_STORY, JSON_TREE_STORY, TERMINAL_TRANSCRIPT_STORY,
  QUERY_PLAN_STORY, DEPENDENCY_GRAPH_STORY, NETWORK_WATERFALL_STORY, COVERAGE_STORY,
  TEST_REPORT_STORY, TIMELINE_VIEW_STORY, DISASSEMBLY_STORY, MEMORY_STORY, PROFILE_STORY,
  FILE_BROWSER_STORY, METRICS_STORY, BINARY_STORY, ACQUISITION_STORY, KEYBOARD_STORY,
  STREAMING_LOG_STORY, EVENT_STREAM_STORY, KEY_VALUE_STORY, JSON_STORY, STACK_STORY,
  MARKDOWN_STORY, FORM_STORY, DIFF_STORY, NAVIGATION_STORY,
]);

/** The whole playground. */
export function Playground({ largeSource, timelineSource, keyValueSource, fileSource, initialStory = OPENING } = {}) {
  const families = useMemo(grouped, []);
  const [selected, setSelected] = useState(initialStory);
  const [activeFamily, setActiveFamily] = useState(() =>
    families.find((family) => family.tags.some((tag) => tag.name === initialStory))?.name
      ?? families.find((family) => family.tags.some((tag) => tag.name === OPENING))?.name
      ?? families[0]?.name);
  const [edited, setEdited] = useState(() => new Map());

  const flow = FLOW_STORIES.includes(selected);
  const opened = flow ? null : edited.get(selected) ?? defaults(selected);
  const contract = flow ? null : component(selected);
  const properties = flow ? [] : rows(selected);
  const change = (name, value) => {
    const next = new Map(edited);
    next.set(selected, { ...opened, props: { ...opened.props, [name]: value } });
    setEdited(next);
  };

  return h(
    Row,
    { gap: 0, grow: true, wrap: true },
    h(Sidebar, { key: 'sidebar', families, selected, activeFamily, onFamily: setActiveFamily, onSelect: setSelected }),
    h(Separator, { key: 'first', orientation: 'vertical' }),
    h(Preview, { key: `preview-${selected}`, name: selected, opened, largeSource, timelineSource, keyValueSource, fileSource, triggers: contract?.triggers ?? [] }),
    h(Separator, { key: 'second', orientation: 'vertical' }),
    h(Inspector, {
      key: 'inspector',
      name: selected,
      properties,
      triggers: contract?.triggers ?? [],
      props: opened?.props ?? {},
      onChange: change,
    }),
  );
}

/** Every component, under the family it belongs to. */
export function Sidebar({ families, selected, activeFamily, onFamily, onSelect }) {
  const [search, setSearch] = useState('');
  const family = families.find((candidate) => candidate.name === activeFamily) ?? families[0];
  const query = search.trim().toLocaleLowerCase();
  const visible = family.tags.filter((tag) => query.length === 0 || tag.name.toLocaleLowerCase().includes(query));
  return h(
    Scroll,
    { width: 'fill', height: 'fill' },
    h(
      List,
      { pad: 1 },
      h(ListSubheader, { key: 'flows', label: 'End-user flows', tooltip: 'whole product states composed from the library' }),
      ...FLOW_STORIES.map((story) => h(ListItemButton, { key: story, label: story, selected: selected === story, onInvoke: () => onSelect(story) })),
      h(ListSubheader, { key: 'components', label: 'Components', tooltip: 'choose one bounded catalogue family' }),
      h(Select, {
        key: 'family', value: family.name,
        choices: families.map((candidate) => ({ value: candidate.name, label: candidate.label })),
        onChange: (event) => { setSearch(''); onFamily(String(event.value)); },
      }),
      h(Entry, { key: 'search', value: search, placeholder: 'Search active family', onChange: (event) => setSearch(String(event.value ?? '').slice(0, 80)) }),
      h(ListSubheader, { key: family.name, label: family.label, tooltip: family.note }),
      ...visible.map((tag) => h(ListItemButton, {
        key: tag.name, label: tag.name, selected: tag.name === selected, onInvoke: () => onSelect(tag.name),
      })),
      ...(visible.length === 0 ? [h(Text, { key: 'none', label: 'No components match this family search.', color: 'text-dim', wrap: true })] : []),
    ),
  );
}

/** The selected component, alive, with the properties currently set on it. */
export function Preview({ name, opened, largeSource, timelineSource, keyValueSource, fileSource, triggers = [] }) {
  const [interactions, setInteractions] = useState([]);
  const sequence = useRef(0);
  const handlers = interactionProps(triggers, (trigger, event) => {
    const interaction = { sequence: ++sequence.current, trigger, detail: interactionDetail(event) };
    setInteractions((current) => [...current, interaction].slice(-INTERACTION_HISTORY));
  });
  return h(
    Column,
    { grow: true, gap: 2, pad: 4 },
    h(Heading, { key: 'title', label: spaced(name), scale: 'title', wrap: true }),
    h(
      Section,
      { key: 'stage', pad: 4, grow: true },
      name === RESOURCE_STATE_STORY
        ? h(ResourceStateStory)
        : name === QUERY_PLAN_STORY
        ? h(QueryPlanStory)
        : name === IMAGE_PULL_STORY
        ? h(ImagePullStory)
        : name === WORKSPACE_FILE_EDIT_STORY
        ? h(WorkspaceFileEditStory)
        : name === EXTENSION_LIFECYCLE_STORY
        ? h(ExtensionLifecycleStory)
        : name === WORKSPACE_LAYOUT_STORY
        ? h(WorkspaceLayoutStory)
        : name === CONTAINER_OPERATIONS_STORY
        ? h(ContainerOperationsStory)
        : name === CONFIRMATION_STORY
        ? h(ConfirmationStory)
        : name === COMMAND_PALETTE_STORY
        ? h(CommandPaletteStory)
        : name === JSON_TREE_STORY
        ? h(JsonTreeStory)
        : name === TERMINAL_TRANSCRIPT_STORY
        ? h(TerminalTranscriptStory)
        : name === DEPENDENCY_GRAPH_STORY
        ? h(DependencyGraphStory)
        : name === NETWORK_WATERFALL_STORY
        ? h(NetworkWaterfallStory)
        : name === COVERAGE_STORY
        ? h(CoverageInspectionStory)
        : name === TEST_REPORT_STORY
        ? h(TestReportStory)
        : name === TIMELINE_VIEW_STORY
        ? h(TimelineInspectionStory)
        : name === DISASSEMBLY_STORY
        ? h(DisassemblyInspectionStory)
        : name === MEMORY_STORY
        ? h(MemoryInspectionStory)
        : name === PROFILE_STORY
        ? h(ProfileInspectionStory)
        : name === FILE_BROWSER_STORY && fileSource
        ? h(FileBrowserStory)
        : name === METRICS_STORY
        ? h(ResourceMetricsStory)
        : name === BINARY_STORY
        ? h(BinaryInspectionStory)
        : name === ACQUISITION_STORY
        ? h(AcquisitionProgressStory)
        : name === DIFF_STORY
        ? h(DiffReviewStory)
        : name === FORM_STORY
        ? h(ValidatedSettingsFormStory)
        : name === KEYBOARD_STORY
        ? h(KeyboardAccessibilityStory)
        : name === STREAMING_LOG_STORY
        ? h(StreamingLogStory)
        : name === EVENT_STREAM_STORY && timelineSource
        ? h(EventStreamStory, { source: timelineSource })
        : name === KEY_VALUE_STORY && keyValueSource
        ? h(KeyValueInspectorStory, { source: keyValueSource })
        : name === MARKDOWN_STORY
        ? h(MarkdownReviewStory)
        : name === JSON_STORY
        ? h(JsonResponseStory)
        : name === STACK_STORY
        ? h(StackTraceStory)
        : name === NAVIGATION_STORY
        ? h(NavigationDialogsStory)
        : name === 'DataTable' && largeSource
        ? h(LargeDataTableStory, { source: largeSource })
        : h(components[name], { ...present(opened.props), ...handlers }, ...opened.children.map(child)),
    ),
    ...(triggers.length === 0
      ? []
      : [
          h(
            Column,
            { key: 'interaction-console', gap: 1 },
            h(
              Row,
              { key: 'heading', align: 'center', gap: 1 },
              h(Text, { key: 'title', label: 'Recent interactions', color: 'text-dim', grow: true }),
              ...(interactions.length === 0
                ? []
                : [h(Button, { key: 'clear', label: 'Clear', variant: 'ghost', onInvoke: () => setInteractions([]) })]),
            ),
            ...(interactions.length === 0
              ? [
                  h(InlineMessage, {
                    key: 'empty',
                    label: `Interact with the preview to inspect ${triggers.map((trigger) => `on${trigger}`).join(', ')}.`,
                    tone: 'neutral',
                  }),
                ]
              : interactions.map((interaction) =>
                  h(InlineMessage, {
                    key: interaction.sequence,
                    label: `#${interaction.sequence} ${interaction.trigger} received${interaction.detail ? ` · ${interaction.detail}` : ''}`,
                    tone: 'positive',
                  }),
                )),
          ),
        ]),
  );
}

/** Real handlers for every interaction the selected component declares. */
export function interactionProps(triggers, receive) {
  return Object.fromEntries(triggers.map((trigger) => [`on${trigger}`, (event) => receive(trigger, event)]));
}

/** A short, finite payload description suitable for the visible event console. */
export function interactionDetail(event) {
  if (event === null || typeof event !== 'object') return '';
  const fields = ['value', 'rows', 'key', 'pressed', 'focused', 'phase', 'x', 'y', 'button'];
  const detail = fields
    .filter((field) => event[field] !== undefined)
    .map((field) => `${field}=${JSON.stringify(boundedValue(event[field]))}`)
    .join(' ');
  return detail.slice(0, 240);
}

/** Bound payload work as well as its visible result: events are extension-controlled input. */
function boundedValue(value, depth = 0) {
  if (typeof value === 'string') return value.length > 80 ? `${value.slice(0, 79)}…` : value;
  if (value === null || typeof value !== 'object') return value;
  if (depth >= 2) return '…';
  if (Array.isArray(value)) {
    const shown = value.slice(0, 3).map((entry) => boundedValue(entry, depth + 1));
    return value.length > shown.length ? [...shown, `… ${value.length - shown.length} more`] : shown;
  }
  const shown = {};
  let count = 0;
  for (const key in value) {
    if (!Object.hasOwn(value, key)) continue;
    count += 1;
    if (count <= 4) shown[key] = boundedValue(value[key], depth + 1);
    if (count === 5) {
      shown['…'] = 'more';
      break;
    }
  }
  return shown;
}

/** One default child, as an element. */
function child(descriptor, index) {
  return h(components[descriptor.tag], { key: `child-${index}`, ...descriptor.props });
}

/** The props as the component takes them; an unset property is simply absent. */
export function present(props) {
  return Object.fromEntries(Object.entries(props).filter(([, value]) => value !== undefined));
}

/** One row per property, grouped, with the control its editor hint asks for. */
export function Inspector({ name, properties, triggers, props, onChange }) {
  const groups = [];
  let current = null;
  for (const row of properties) {
    const key = `${row.editable ? 'set' : 'read'}:${row.group}`;
    if (current === null || current.key !== key) {
      current = { key, group: row.group, editable: row.editable, rows: [] };
      groups.push(current);
    }
    current.rows.push(row);
  }
  return h(
    Scroll,
    { width: 'fill', height: 'fill' },
    h(
      Column,
      { pad: 3, gap: 2 },
      h(Heading, { key: 'title', label: `${name} properties`, scale: 'caption', wrap: true }),
      h(Text, { key: 'note', label: notes.values, color: 'text-dim', wrap: true }),
      ...groups.flatMap((group) => [
        h(ListSubheader, { key: `group-${group.key}`, label: group.group }),
        ...group.rows.map((row) =>
          h(Field, { key: row.name, row, value: props[row.name], onChange }),
        ),
      ]),
      ...(triggers.length === 0
        ? []
        : [
            h(ListSubheader, { key: 'interactions', label: 'interactions' }),
            ...triggers.map((trigger) =>
              h(Text, { key: `trigger-${trigger}`, label: `on${trigger}`, color: 'text-dim' }),
            ),
          ]),
    ),
  );
}

/** One property, with the control that edits it. */
export function Field({ row, value, onChange }) {
  return h(
    Row,
    { gap: 2, align: 'center' },
    h(Text, { key: 'name', label: row.name, tooltip: row.note, width: { chars: 12 } }),
    h(React.Fragment, { key: 'control' }, ...controls(row, value, onChange)),
  );
}

/** The controls a property's editor hint asks for, already wired to `onChange`. */
function controls(row, value, onChange) {
  switch (row.editor) {
    case 'text':
      return [
        h(Entry, {
          key: 'value',
          value: value === undefined ? '' : String(value),
          placeholder: row.note,
          onChange: (event) => onChange(row.name, event.value),
        }),
      ];
    case 'switch':
      return [
        h(Switch, {
          key: 'value',
          checked: Boolean(value),
          onToggle: (event) => onChange(row.name, event.value === null ? !value : Boolean(event.value)),
        }),
      ];
    case 'enum':
      return [
        h(Select, {
          key: 'value',
          choices: row.members,
          value: value === undefined ? '' : String(value),
          onChange: (event) => onChange(row.name, event.value),
        }),
      ];
    case 'number':
      return [
        h(NumberEntry, {
          key: 'value',
          value: typeof value === 'number' ? value : 0,
          onChange: (event) => onChange(row.name, Number(event.value)),
        }),
      ];
    case 'length':
    case 'edges': {
      const mode = modeOf(value);
      const amount = amountOf(value);
      return [
        h(Select, {
          key: 'mode',
          choices: row.modes,
          value: mode,
          onChange: (event) => onChange(row.name, lengthValue(event.value, amount)),
        }),
        ...(mode === 'step' || mode === 'chars'
          ? [
              h(NumberEntry, {
                key: 'amount',
                value: amount,
                minimum: 0,
                maximum: mode === 'step' ? row.maximum : 120,
                step: 1,
                onChange: (event) => onChange(row.name, lengthValue(mode, Number(event.value))),
              }),
            ]
          : []),
      ];
    }
    default:
      return [h(Text, { key: 'value', label: `${row.editor}: set in code`, color: 'text-faint' })];
  }
}
