// The playground: pick a component on the left, see it in the middle, change
// its properties on the right.
//
// Every list here is the catalogue's; nothing about the component library is
// spelled out in this file.

import React from 'react';
import {
  Column,
  Entry,
  Heading,
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

import { grouped, notes } from './catalogue.js';
import { OPENING, defaults, spaced } from './defaults.js';
import { amountOf, lengthValue, modeOf, rows } from './editors.js';

const { createElement: h, useMemo, useState } = React;

/** The whole playground. */
export function Playground() {
  const families = useMemo(grouped, []);
  const properties = useMemo(rows, []);
  const [selected, setSelected] = useState(OPENING);
  const [edited, setEdited] = useState(() => new Map());

  const opened = edited.get(selected) ?? defaults(selected);
  const change = (name, value) => {
    const next = new Map(edited);
    next.set(selected, { ...opened, props: { ...opened.props, [name]: value } });
    setEdited(next);
  };

  return h(
    Row,
    { gap: 0, grow: true },
    h(Sidebar, { key: 'sidebar', families, selected, onSelect: setSelected }),
    h(Separator, { key: 'first', orientation: 'vertical' }),
    h(Preview, { key: 'preview', name: selected, opened }),
    h(Separator, { key: 'second', orientation: 'vertical' }),
    h(Inspector, { key: 'inspector', name: selected, properties, props: opened.props, onChange: change }),
  );
}

/** Every component, under the family it belongs to. */
export function Sidebar({ families, selected, onSelect }) {
  return h(
    Scroll,
    { width: { chars: 26 }, height: 'fill' },
    h(
      List,
      { pad: 1 },
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
export function Preview({ name, opened }) {
  return h(
    Column,
    { grow: true, gap: 2, pad: 4 },
    h(Heading, { key: 'title', label: spaced(name), scale: 'title' }),
    h(
      Section,
      { key: 'stage', pad: 4, grow: true },
      h(components[name], present(opened.props), ...opened.children.map(child)),
    ),
  );
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
export function Inspector({ name, properties, props, onChange }) {
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
    { width: { chars: 40 }, height: 'fill' },
    h(
      Column,
      { pad: 3, gap: 2 },
      h(Heading, { key: 'title', label: `${name} properties`, scale: 'caption' }),
      h(Text, { key: 'note', label: notes.propsPerTag, color: 'text-dim', wrap: true }),
      ...groups.flatMap((group) => [
        h(ListSubheader, { key: `group-${group.key}`, label: group.group }),
        ...group.rows.map((row) =>
          h(Field, { key: row.name, row, value: props[row.name], onChange }),
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
