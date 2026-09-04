// @ts-nocheck -- legacy story typing is migrated incrementally.
import React, { useState } from 'react';
import { Button, Column, Heading, InlineMessage, Row, Scroll, Separator, Text } from '@husklet/react';

export const DRAG_REORDER_STORY = 'Drag and keyboard reorder';
export const EVENT_LIMIT = 6;
export const initialItems = Object.freeze([
  { id: 'build', label: 'Build' }, { id: 'test', label: 'Test' }, { id: 'publish', label: 'Publish' },
]);

export function reorder(items, source, target) {
  const from = items.findIndex(({ id }) => id === source);
  const to = items.findIndex(({ id }) => id === target);
  if (from < 0 || to < 0 || from === to) return items;
  const next = [...items];
  const [moved] = next.splice(from, 1);
  next.splice(to, 0, moved);
  return next;
}

export function DragReorderStory() {
  const [items, setItems] = useState(initialItems);
  const [source, setSource] = useState(null);
  const [events, setEvents] = useState([]);
  const record = (message) => setEvents((current) => [...current, message].slice(-EVENT_LIMIT));
  const move = (id, offset, method) => {
    const index = items.findIndex((item) => item.id === id);
    const target = items[index + offset];
    if (!target) return;
    setItems((current) => reorder(current, id, target.id));
    record(`${method}: ${id} → ${target.id}`);
  };
  return (
    <Scroll width={'fill'} height={'fill'}>
      <Column gap={2} grow={true}>
        <Heading label={'Reorder a release pipeline'} scale={'title'} />
        <Text
          label={'Drag a card onto another card, or use Move up and Move down. Both paths apply the same bounded reorder.'}
          wrap={true} />
        {items.map((item, index) => <Column
          key={item.id}
          label={item.label}
          onDrag={() => { setSource(item.id); record(`Drag source: ${item.id}`); }}
          onDrop={(event) => {
            if (!source) { record(`Drop ignored on ${item.id}: no source`); return; }
            setItems((current) => reorder(current, source, item.id));
            record(`Drop: ${source} → ${item.id} (node ${event.source})`);
            setSource(null);
          }}>
          <Heading label={`${index + 1}. ${item.label}`} scale={'body'} />
          <Text label={`stable id: ${item.id}`} color={'text-dim'} />
          <Row gap={1} wrap={true}>
            <Button
              label={`↑ ${item.label}`}
              tooltip={`Move ${item.label} up`}
              enabled={index !== 0}
              onInvoke={() => move(item.id, -1, 'Keyboard')} />
            <Button
              label={`↓ ${item.label}`}
              tooltip={`Move ${item.label} down`}
              enabled={index !== items.length - 1}
              onInvoke={() => move(item.id, 1, 'Keyboard')} />
          </Row>
          <Separator />
        </Column>)}
        <InlineMessage
          label={source ? `Dragging ${source}; choose a target.` : 'Ready to reorder.'}
          tone={'neutral'} />
        <Column label={'Inspector metadata'} gap={1}>
          <Heading label={'Inspector metadata'} scale={'body'} />
          <Text
            label={`${items.length} bounded items · ${events.length}/${EVENT_LIMIT} events`}
            color={'text-dim'} />
          <Text label={`Order: ${items.map(({ id }) => id).join(' → ')}`} wrap={true} />
          {(events.length ? events : ['No interactions yet.']).map((event, index) => <Text key={`${index}:${event}`} label={event} wrap={true} />)}
        </Column>
      </Column>
    </Scroll>
  );
}
