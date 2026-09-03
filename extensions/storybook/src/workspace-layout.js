import React, { useState } from 'react';
import {
  Button, Card, CardContent, CardHeader, Column, ConfirmAction, Heading, InlineMessage,
  List, ListItemButton, Row, Splitter, Text, Tree, TreeItem,
} from '@husklet/react';

const { createElement: h } = React;

export const WORKSPACE_LAYOUT_STORY = 'Workspace layout control';
export const PANE_LIMIT = 12;
export const TITLE_LIMIT = 48;

const clean = (value, limit) => String(value ?? '').replace(/[\r\n\t]/g, ' ').slice(0, limit);
const CLEAN_SLOT = /^[A-Za-z0-9][A-Za-z0-9._:-]{0,79}$/;

export function boundedPanes(panes) {
  return panes.slice(0, PANE_LIMIT).map((pane) => ({
    slot: CLEAN_SLOT.test(pane.slot ?? '') ? pane.slot : '',
    title: clean(pane.title, TITLE_LIMIT),
    occupant: ['terminal', 'extension', 'empty'].includes(pane.occupant) ? pane.occupant : 'empty',
  })).filter(({ slot }) => slot);
}

const initial = boundedPanes([
  { slot: 'pane-terminal-1', title: 'API shell', occupant: 'terminal' },
  { slot: 'pane-extension-2', title: 'Container operations', occupant: 'extension' },
  { slot: 'pane-empty-3', title: 'Available pane', occupant: 'empty' },
]);

export function WorkspaceLayoutStory() {
  const [panes, setPanes] = useState(initial);
  const [selectedSlot, setSelectedSlot] = useState(initial[0].slot);
  const [orientation, setOrientation] = useState('horizontal');
  const [status, setStatus] = useState('Select a stable pane slot before changing the layout.');
  const selected = panes.find(({ slot }) => slot === selectedSlot) ?? panes[0];
  const neighbor = panes.find(({ slot }) => slot !== selected?.slot) ?? selected;
  const split = (nextOrientation) => {
    if (panes.length >= PANE_LIMIT) {
      setStatus(`Layout already contains the ${PANE_LIMIT}-pane limit.`);
      return;
    }
    const pane = { slot: `pane-new-${panes.length + 1}`, title: 'New terminal', occupant: 'terminal' };
    setOrientation(nextOrientation);
    setPanes((current) => [...current, pane]);
    setSelectedSlot(pane.slot);
    setStatus(`Split ${selected.slot} ${nextOrientation === 'horizontal' ? 'beside' : 'below'} into ${pane.slot}.`);
  };
  const switchOccupant = () => {
    setPanes((current) => current.map((pane) => pane.slot === selected.slot
      ? { ...pane, occupant: pane.occupant === 'terminal' ? 'extension' : 'terminal' } : pane));
    setStatus(`Switched occupant in immutable slot ${selected.slot}.`);
  };

  return h(Column, { gap: 2, grow: true },
    h(Heading, { label: 'Workspace layout control', scale: 'title' }),
    h(Text, { label: 'Pane mutations target stable slots. The visible topology and occupant kind update together.', wrap: true }),
    h(Row, { gap: 2, wrap: true },
      h(List, { label: 'Pane slots' }, ...panes.map((pane) => h(ListItemButton, {
        key: pane.slot, label: `${pane.title} · ${pane.occupant}`, selected: pane.slot === selected.slot,
        onInvoke: () => { setSelectedSlot(pane.slot); setStatus(`Selected immutable slot ${pane.slot}.`); },
      }))),
      h(Column, { gap: 2, grow: true },
        h(Tree, { label: 'Layout topology' }, h(TreeItem, { label: `root · ${orientation}`, expanded: true },
          ...panes.map((pane) => h(TreeItem, { key: pane.slot, label: `${pane.slot} · ${pane.occupant}` })))),
        h(Splitter, { orientation, position: 320, grow: true },
          h(Card, { label: selected.title },
            h(CardHeader, { label: selected.title, detail: selected.slot }),
            h(CardContent, {}, h(Text, { label: selected.occupant }))),
          h(Card, { label: neighbor.title },
            h(CardHeader, { label: neighbor.title, detail: neighbor.slot }),
            h(CardContent, {}, h(Text, { label: neighbor.occupant })))))),
    h(Row, { gap: 2, wrap: true },
      h(Button, { label: 'Split beside', onInvoke: () => split('horizontal') }),
      h(Button, { label: 'Split below', onInvoke: () => split('vertical') }),
      h(Button, { label: 'Switch occupant', onInvoke: switchOccupant }),
      h(ConfirmAction, { authorityKey: selected.slot, label: 'Close pane', confirmLabel: 'Confirm close', question: `Close ${selected.slot}?`, onConfirm: async () => setStatus(`Close confirmed for immutable slot ${selected.slot}.`) })),
    h(InlineMessage, { label: status, tone: 'neutral' }));
}
