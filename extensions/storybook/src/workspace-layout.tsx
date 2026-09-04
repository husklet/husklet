import React, { useState } from 'react';
import {
  Button, Card, CardContent, CardHeader, Column, ConfirmAction, Heading, InlineMessage,
  List, ListItemButton, Row, Scroll, Select, Splitter, Text, Tree, TreeItem,
} from '@husklet/react';


export const WORKSPACE_LAYOUT_STORY = 'Workspace layout control';
export const PANE_LIMIT = 12;
export const TITLE_LIMIT = 48;
export const EVENT_LIMIT = 6;
export const retainEvents = (events: readonly string[], next: string): string[] => [...events, next].slice(-EVENT_LIMIT);

export type PaneOccupant = 'terminal' | 'extension' | 'empty';
export type SplitOrientation = 'horizontal' | 'vertical';
export interface PaneInput { slot?: unknown; title?: unknown; occupant?: unknown; tab?: unknown; provider?: unknown; }
export interface BoundedPane { slot: string; title: string; occupant: PaneOccupant; tab: string; provider: string; }

const clean = (value: unknown, limit: number) => String(value ?? '').replace(/[\r\n\t]/g, ' ').slice(0, limit);
const CLEAN_SLOT = /^[A-Za-z0-9][A-Za-z0-9._:-]{0,79}$/;
const isPaneOccupant = (value: unknown): value is PaneOccupant =>
  value === 'terminal' || value === 'extension' || value === 'empty';

export function boundedPanes(panes: readonly PaneInput[]): BoundedPane[] {
  return panes.slice(0, PANE_LIMIT).map((pane) => {
    const slot = String(pane.slot ?? '');
    const tab = String(pane.tab ?? '');
    return {
      slot: CLEAN_SLOT.test(slot) ? slot : '',
      title: clean(pane.title, TITLE_LIMIT),
      occupant: isPaneOccupant(pane.occupant) ? pane.occupant : 'empty',
      tab: CLEAN_SLOT.test(tab) ? tab : 'shells',
      provider: pane.occupant === 'extension' ? clean(pane.provider, TITLE_LIMIT) : '',
    };
  }).filter(({ slot }) => slot);
}

const initial = boundedPanes([
  { slot: 'pane-terminal-1', title: 'API shell', occupant: 'terminal', tab: 'shells' },
  { slot: 'pane-extension-2', title: 'Container operations', occupant: 'extension', provider: 'top/containers', tab: 'shells' },
  { slot: 'pane-terminal-3', title: 'Logs', occupant: 'terminal', tab: 'observability' },
]);

export function WorkspaceLayoutStory() {
  const [panes, setPanes] = useState(initial);
  const [selectedSlot, setSelectedSlot] = useState(initial[0]!.slot);
  const [orientation, setOrientation] = useState<SplitOrientation>('horizontal');
  const [activeTab, setActiveTab] = useState('shells');
  const [focusedSlot, setFocusedSlot] = useState(initial[0]!.slot);
  const [events, setEvents] = useState<string[]>(['Ready: shells active; pane-terminal-1 focused.']);
  const record = (message: string) => setEvents((current) => retainEvents(current, message));
  const visible = panes.filter(({ tab }) => tab === activeTab);
  const selected = panes.find(({ slot }) => slot === selectedSlot) ?? panes[0];
  const neighbor = visible.find(({ slot }) => slot !== selected?.slot) ?? selected;
  const split = (nextOrientation: SplitOrientation) => {
    if (!selected) return;
    if (panes.length >= PANE_LIMIT) {
      record(`Layout already contains the ${PANE_LIMIT}-pane limit.`);
      return;
    }
    const pane: BoundedPane = { slot: `pane-new-${panes.length + 1}`, title: 'New terminal', occupant: 'terminal', tab: activeTab, provider: '' };
    setOrientation(nextOrientation);
    setPanes((current) => [...current, pane]);
    setSelectedSlot(pane.slot);
    record(`Split ${selected.slot} ${nextOrientation === 'horizontal' ? 'beside' : 'below'} into ${pane.slot}.`);
  };
  const switchOccupant = () => {
    if (!selected) return;
    setPanes((current) => current.map((pane) => pane.slot === selected.slot
      ? pane.occupant === 'terminal'
        ? { ...pane, occupant: 'extension', provider: 'top/containers' }
        : { ...pane, occupant: 'terminal', provider: '' }
      : pane));
    record(`Chooser switched ${selected.slot} without changing its slot or focus.`);
  };

  return (
    <Scroll width={'fill'} height={'fill'}>
      <Column gap={2} grow={true}>
        <Heading label={'Workspace layout control'} scale={'title'} />
        <Text
          label={'Tabs, nested splits, keyboard focus, and chooser occupant changes retain immutable pane slots.'}
          wrap={true} />
        <Heading label={'Active tab'} scale={'body'} />
        <Select
          value={activeTab}
          choices={[
            { value: 'shells', label: 'Shells' }, { value: 'observability', label: 'Observability' },
          ]}
          onChange={(event) => {
            const tab = String(event.value);
            setActiveTab(tab);
            const first = panes.find((pane) => pane.tab === tab);
            if (first) setSelectedSlot(first.slot);
            record(`Activated tab ${tab}.`);
          }} />
        <Row gap={2} wrap={true}>
          <Column gap={1}>
            <Heading label={'Pane slots'} scale={'body'} />
            <List>
              {visible.map((pane) => <ListItemButton
                key={pane.slot}
                label={`${pane.title} · ${pane.occupant}${pane.provider ? ` · ${pane.provider}` : ''}${pane.slot === focusedSlot ? ' · focused' : ''}`}
                variant={pane.slot === selected?.slot ? 'filled' : 'plain'}
                onInvoke={() => { setSelectedSlot(pane.slot); record(`Selected immutable slot ${pane.slot}.`); }} />)}
            </List>
          </Column>
          <Column gap={2} grow={true}>
            <Heading label={'Layout topology'} scale={'body'} />
            <Tree>
              <TreeItem label={'workspace tabs'} expanded={true}>
                <TreeItem
                  label={`Shells · ${orientation} split${activeTab === 'shells' ? ' · active' : ''}`}
                  expanded={true}>
                  <TreeItem label={'nested horizontal split'} expanded={true}>
                    {panes.filter(({ tab }) => tab === 'shells').map((pane) => <TreeItem
                      key={pane.slot}
                      label={`${pane.slot} · ${pane.occupant}${pane.slot === focusedSlot ? ' · focused' : ''}`} />)}
                  </TreeItem>
                </TreeItem>
                <TreeItem
                  label={`Observability${activeTab === 'observability' ? ' · active' : ''}`}
                  expanded={true}>
                  {panes.filter(({ tab }) => tab === 'observability').map((pane) => <TreeItem
                    key={pane.slot}
                    label={`${pane.slot} · ${pane.occupant}${pane.slot === focusedSlot ? ' · focused' : ''}`} />)}
                </TreeItem>
              </TreeItem>
            </Tree>
            {selected && neighbor ? <Splitter orientation={orientation} position={140} grow={true}>
              <Card label={selected.title}>
                <CardHeader label={selected.title} detail={selected.slot} />
                <CardContent>
                  <Text label={selected.provider || selected.occupant} />
                </CardContent>
              </Card>
              <Card label={neighbor.title}>
                <CardHeader label={neighbor.title} detail={neighbor.slot} />
                <CardContent>
                  <Text label={neighbor.occupant} />
                </CardContent>
              </Card>
            </Splitter> : null}
          </Column>
        </Row>
        <Row gap={2} wrap={true}>
          <Button label={'Split beside'} onInvoke={() => split('horizontal')} />
          <Button label={'Split below'} onInvoke={() => split('vertical')} />
          <Button
            label={'Focus selected pane'}
            enabled={Boolean(selected)}
            onInvoke={() => { setFocusedSlot(selected.slot); record(`Focused ${selected.slot} in ${activeTab}.`); }} />
          <Button
            label={'Open pane chooser'}
            enabled={Boolean(selected)}
            tooltip={'Switch terminal and extension content in this stable pane'}
            onInvoke={switchOccupant} />
          {selected ? <ConfirmAction
            authorityKey={selected.slot}
            label={'Close pane'}
            confirmLabel={'Confirm close'}
            question={`Close ${selected.slot}?`}
            onConfirm={async () => record(`Close confirmed for immutable slot ${selected.slot}.`)} /> : null}
        </Row>
        <InlineMessage label={events.at(-1)} tone={'neutral'} />
        <Column gap={1}>
          <Heading label={'Bounded layout events'} scale={'body'} />
          <Text
            label={`${events.length}/${EVENT_LIMIT} recent events`}
            color={'text-dim'} />
          {events.map((event, index) => <Text key={`${index}:${event}`} label={event} wrap={true} />)}
        </Column>
      </Column>
    </Scroll>
  );
}
