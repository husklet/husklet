import React, { useState } from 'react';
import {
  Button, Card, CardContent, CardHeader, Column, ConfirmAction, Heading, InlineMessage,
  List, ListItemButton, Row, Scroll, Select, Splitter, Text, Tree, TreeItem,
} from '@husklet/react';

const { createElement: h } = React;

export const WORKSPACE_LAYOUT_STORY = 'Workspace layout control';
export const PANE_LIMIT = 12;
export const TITLE_LIMIT = 48;
export const EVENT_LIMIT = 6;
export const retainEvents = (events, next) => [...events, next].slice(-EVENT_LIMIT);

const clean = (value, limit) => String(value ?? '').replace(/[\r\n\t]/g, ' ').slice(0, limit);
const CLEAN_SLOT = /^[A-Za-z0-9][A-Za-z0-9._:-]{0,79}$/;

export function boundedPanes(panes) {
  return panes.slice(0, PANE_LIMIT).map((pane) => ({
    slot: CLEAN_SLOT.test(pane.slot ?? '') ? pane.slot : '',
    title: clean(pane.title, TITLE_LIMIT),
    occupant: ['terminal', 'extension', 'empty'].includes(pane.occupant) ? pane.occupant : 'empty',
    tab: CLEAN_SLOT.test(pane.tab ?? '') ? pane.tab : 'shells',
    provider: pane.occupant === 'extension' ? clean(pane.provider, TITLE_LIMIT) : '',
  })).filter(({ slot }) => slot);
}

const initial = boundedPanes([
  { slot: 'pane-terminal-1', title: 'API shell', occupant: 'terminal', tab: 'shells' },
  { slot: 'pane-extension-2', title: 'Container operations', occupant: 'extension', provider: 'workspace-manager/containers', tab: 'shells' },
  { slot: 'pane-terminal-3', title: 'Logs', occupant: 'terminal', tab: 'observability' },
]);

export function WorkspaceLayoutStory() {
  const [panes, setPanes] = useState(initial);
  const [selectedSlot, setSelectedSlot] = useState(initial[0].slot);
  const [orientation, setOrientation] = useState('horizontal');
  const [activeTab, setActiveTab] = useState('shells');
  const [focusedSlot, setFocusedSlot] = useState(initial[0].slot);
  const [events, setEvents] = useState(['Ready: shells active; pane-terminal-1 focused.']);
  const record = (message) => setEvents((current) => retainEvents(current, message));
  const visible = panes.filter(({ tab }) => tab === activeTab);
  const selected = panes.find(({ slot }) => slot === selectedSlot) ?? panes[0];
  const neighbor = visible.find(({ slot }) => slot !== selected?.slot) ?? selected;
  const split = (nextOrientation) => {
    if (panes.length >= PANE_LIMIT) {
      setStatus(`Layout already contains the ${PANE_LIMIT}-pane limit.`);
      return;
    }
    const pane = { slot: `pane-new-${panes.length + 1}`, title: 'New terminal', occupant: 'terminal', tab: activeTab, provider: '' };
    setOrientation(nextOrientation);
    setPanes((current) => [...current, pane]);
    setSelectedSlot(pane.slot);
    record(`Split ${selected.slot} ${nextOrientation === 'horizontal' ? 'beside' : 'below'} into ${pane.slot}.`);
  };
  const switchOccupant = () => {
    setPanes((current) => current.map((pane) => pane.slot === selected.slot
      ? pane.occupant === 'terminal'
        ? { ...pane, occupant: 'extension', provider: 'workspace-manager/containers' }
        : { ...pane, occupant: 'terminal', provider: '' }
      : pane));
    record(`Chooser switched ${selected.slot} without changing its slot or focus.`);
  };

  return h(Scroll, { width: 'fill', height: 'fill' }, h(Column, { gap: 2, grow: true },
    h(Heading, { label: 'Workspace layout control', scale: 'title' }),
    h(Text, { label: 'Tabs, nested splits, keyboard focus, and chooser occupant changes retain immutable pane slots.', wrap: true }),
    h(Select, { label: 'Active tab', value: activeTab, choices: [
      { value: 'shells', label: 'Shells' }, { value: 'observability', label: 'Observability' },
    ], onChange: (event) => {
      const tab = String(event.value);
      setActiveTab(tab);
      const first = panes.find((pane) => pane.tab === tab);
      if (first) setSelectedSlot(first.slot);
      record(`Activated tab ${tab}.`);
    } }),
    h(Row, { gap: 2, wrap: true },
      h(List, { label: 'Pane slots' }, ...visible.map((pane) => h(ListItemButton, {
        key: pane.slot, label: `${pane.title} · ${pane.occupant}${pane.provider ? ` · ${pane.provider}` : ''}${pane.slot === focusedSlot ? ' · focused' : ''}`, selected: pane.slot === selected.slot,
        onInvoke: () => { setSelectedSlot(pane.slot); record(`Selected immutable slot ${pane.slot}.`); },
      }))),
      h(Column, { gap: 2, grow: true },
        h(Tree, { label: 'Layout topology' },
          h(TreeItem, { label: 'workspace tabs', expanded: true },
            h(TreeItem, { label: `Shells · ${orientation} split${activeTab === 'shells' ? ' · active' : ''}`, expanded: true },
              h(TreeItem, { label: 'nested horizontal split', expanded: true },
                ...panes.filter(({ tab }) => tab === 'shells').map((pane) => h(TreeItem, { key: pane.slot, label: `${pane.slot} · ${pane.occupant}${pane.slot === focusedSlot ? ' · focused' : ''}` })))),
            h(TreeItem, { label: `Observability${activeTab === 'observability' ? ' · active' : ''}`, expanded: true },
              ...panes.filter(({ tab }) => tab === 'observability').map((pane) => h(TreeItem, { key: pane.slot, label: `${pane.slot} · ${pane.occupant}${pane.slot === focusedSlot ? ' · focused' : ''}` }))))),
        h(Splitter, { orientation, position: 140, grow: true },
          h(Card, { label: selected.title },
            h(CardHeader, { label: selected.title, detail: selected.slot }),
            h(CardContent, {}, h(Text, { label: selected.provider || selected.occupant }))),
          h(Card, { label: neighbor.title },
            h(CardHeader, { label: neighbor.title, detail: neighbor.slot }),
            h(CardContent, {}, h(Text, { label: neighbor.occupant })))))),
    h(Row, { gap: 2, wrap: true },
      h(Button, { label: 'Split beside', onInvoke: () => split('horizontal') }),
      h(Button, { label: 'Split below', onInvoke: () => split('vertical') }),
      h(Button, { label: 'Focus selected pane', onInvoke: () => { setFocusedSlot(selected.slot); record(`Focused ${selected.slot} in ${activeTab}.`); } }),
      h(Button, { label: 'Open pane chooser', tooltip: 'Switch terminal and extension content in this stable pane', onInvoke: switchOccupant }),
      h(ConfirmAction, { authorityKey: selected.slot, label: 'Close pane', confirmLabel: 'Confirm close', question: `Close ${selected.slot}?`, onConfirm: async () => record(`Close confirmed for immutable slot ${selected.slot}.`) })),
    h(InlineMessage, { label: events.at(-1), tone: 'neutral' }),
    h(Column, { label: 'Bounded layout events', gap: 1 },
      h(Text, { label: `${events.length}/${EVENT_LIMIT} recent events`, color: 'text-dim' }),
      ...events.map((event, index) => h(Text, { key: `${index}:${event}`, label: event, wrap: true }))))
  );
}
