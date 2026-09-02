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

const { createElement: h, useMemo, useRef, useState } = React;

const INTERACTION_HISTORY = 5;

/** The whole playground. */
export function Playground({ largeSource, timelineSource, keyValueSource, fileSource, initialStory = OPENING } = {}) {
  const families = useMemo(grouped, []);
  const [selected, setSelected] = useState(initialStory);
  const [edited, setEdited] = useState(() => new Map());

  const flow = selected === ACQUISITION_STORY || selected === FORM_STORY || selected === KEYBOARD_STORY
    || selected === NAVIGATION_STORY || selected === STREAMING_LOG_STORY || selected === EVENT_STREAM_STORY
    || selected === KEY_VALUE_STORY || selected === DIFF_STORY || selected === MARKDOWN_STORY
    || selected === JSON_STORY || selected === STACK_STORY || selected === BINARY_STORY
    || selected === METRICS_STORY || selected === FILE_BROWSER_STORY || selected === PROFILE_STORY
    || selected === MEMORY_STORY || selected === DISASSEMBLY_STORY;
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
    h(Sidebar, { key: 'sidebar', families, selected, onSelect: setSelected }),
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
export function Sidebar({ families, selected, onSelect }) {
  return h(
    Scroll,
    { width: 'fill', height: 'fill' },
    h(
      List,
      { pad: 1 },
      h(ListSubheader, { key: 'flows', label: 'End-user flows', tooltip: 'whole product states composed from the library' }),
      h(ListItemButton, { key: DISASSEMBLY_STORY, label: DISASSEMBLY_STORY, selected: selected === DISASSEMBLY_STORY, onInvoke: () => onSelect(DISASSEMBLY_STORY) }),
      h(ListItemButton, { key: MEMORY_STORY, label: MEMORY_STORY, selected: selected === MEMORY_STORY, onInvoke: () => onSelect(MEMORY_STORY) }),
      h(ListItemButton, { key: PROFILE_STORY, label: PROFILE_STORY, selected: selected === PROFILE_STORY, onInvoke: () => onSelect(PROFILE_STORY) }),
      h(ListItemButton, {
        key: FILE_BROWSER_STORY,
        label: FILE_BROWSER_STORY,
        selected: selected === FILE_BROWSER_STORY,
        onInvoke: () => onSelect(FILE_BROWSER_STORY),
      }),
      h(ListItemButton, {
        key: METRICS_STORY,
        label: METRICS_STORY,
        selected: selected === METRICS_STORY,
        onInvoke: () => onSelect(METRICS_STORY),
      }),
      h(ListItemButton, {
        key: BINARY_STORY,
        label: BINARY_STORY,
        selected: selected === BINARY_STORY,
        onInvoke: () => onSelect(BINARY_STORY),
      }),
      h(ListItemButton, {
        key: ACQUISITION_STORY,
        label: ACQUISITION_STORY,
        selected: selected === ACQUISITION_STORY,
        onInvoke: () => onSelect(ACQUISITION_STORY),
      }),
      h(ListItemButton, {
        key: KEYBOARD_STORY,
        label: KEYBOARD_STORY,
        selected: selected === KEYBOARD_STORY,
        onInvoke: () => onSelect(KEYBOARD_STORY),
      }),
      h(ListItemButton, {
        key: STREAMING_LOG_STORY,
        label: STREAMING_LOG_STORY,
        selected: selected === STREAMING_LOG_STORY,
        onInvoke: () => onSelect(STREAMING_LOG_STORY),
      }),
      h(ListItemButton, {
        key: EVENT_STREAM_STORY,
        label: EVENT_STREAM_STORY,
        selected: selected === EVENT_STREAM_STORY,
        onInvoke: () => onSelect(EVENT_STREAM_STORY),
      }),
      h(ListItemButton, {
        key: KEY_VALUE_STORY,
        label: KEY_VALUE_STORY,
        selected: selected === KEY_VALUE_STORY,
        onInvoke: () => onSelect(KEY_VALUE_STORY),
      }),
      h(ListItemButton, {
        key: JSON_STORY,
        label: JSON_STORY,
        selected: selected === JSON_STORY,
        onInvoke: () => onSelect(JSON_STORY),
      }),
      h(ListItemButton, {
        key: STACK_STORY,
        label: STACK_STORY,
        selected: selected === STACK_STORY,
        onInvoke: () => onSelect(STACK_STORY),
      }),
      h(ListItemButton, {
        key: MARKDOWN_STORY,
        label: MARKDOWN_STORY,
        selected: selected === MARKDOWN_STORY,
        onInvoke: () => onSelect(MARKDOWN_STORY),
      }),
      h(ListItemButton, {
        key: FORM_STORY,
        label: FORM_STORY,
        selected: selected === FORM_STORY,
        onInvoke: () => onSelect(FORM_STORY),
      }),
      h(ListItemButton, {
        key: DIFF_STORY,
        label: DIFF_STORY,
        selected: selected === DIFF_STORY,
        onInvoke: () => onSelect(DIFF_STORY),
      }),
      h(ListItemButton, {
        key: NAVIGATION_STORY,
        label: NAVIGATION_STORY,
        selected: selected === NAVIGATION_STORY,
        onInvoke: () => onSelect(NAVIGATION_STORY),
      }),
      ...families.flatMap((family) => [
        h(ListSubheader, { key: family.name, label: family.label, tooltip: family.note }),
        ...family.tags.map((tag) =>
          h(ListItemButton, {
            key: tag.name,
            label: tag.name,
            selected: tag.name === selected,
            onInvoke: () => onSelect(tag.name),
          }),
        ),
      ]),
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
      name === DISASSEMBLY_STORY
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
