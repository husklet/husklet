import React from 'react';
import { Accordion, AccordionDetails, AccordionSummary, Button, Column, CommandPalette, Heading, InlineMessage, Menu, MenuItem, Popover, Row, Text } from '@husklet/react';

const { useState } = React;

export const NAVIGATION_STORY = 'Navigation and transient UI';

/** Stateful navigation and dialog interactions that isolated component previews cannot teach. */
export function NavigationDialogsStory() {
  const [expanded, setExpanded] = useState(true);
  const [open, setOpen] = useState(false);
  const [event, setEvent] = useState('No navigation event yet.');
  const [query, setQuery] = useState('');
  return (
    <Column gap={3}>
      <Heading label={'Navigation and transient UI'} scale={'title'} wrap={true} />
      <Text
        label={'Expand details, open the action menu, choose an item, or dismiss it.'}
        wrap={true} />
      <CommandPalette
        value={query}
        placeholder={'Run a command…'}
        gap={1}
        onChange={(report) => setQuery(String(report.value ?? ''))}
        onSubmit={() => setEvent(`Command submitted: ${query || 'none'}.`)}>
        <MenuItem
          label={'Open terminal'}
          icon={'utilities-terminal-symbolic'}
          onInvoke={() => setEvent('Open terminal selected.')} />
        <MenuItem
          label={'Create workspace'}
          icon={'list-add-symbolic'}
          onInvoke={() => setEvent('Create workspace selected.')} />
      </CommandPalette>
      <Accordion
        label={'Deployment details'}
        expanded={expanded}
        onExpand={(next) => {
          const value = Boolean(next.value);
          setExpanded(value);
          setEvent(value ? 'Deployment details expanded.' : 'Deployment details collapsed.');
        }}>
        <AccordionSummary label={'Deployment details'} icon={'info'} />
        <AccordionDetails gap={1}>
          <Text label={'Three replicas · healthy'} wrap={true} />
          <Button
            label={'Open actions'}
            onInvoke={() => { setOpen(true); setEvent('Action menu opened.'); }} />
        </AccordionDetails>
      </Accordion>
      {open ? [<Popover
        key={'actions'}
        visible={true}
        onClose={() => { setOpen(false); setEvent('Action menu dismissed.'); }}>
        <Menu gap={1}>
          <MenuItem
            label={'View logs'}
            icon={'document-open'}
            onInvoke={() => { setOpen(false); setEvent('View logs selected.'); }} />
          <MenuItem label={'Restart unavailable'} enabled={false} />
        </Menu>
      </Popover>] : []}
      <Row gap={1} wrap={true}>
        <InlineMessage label={event} tone={'neutral'} />
      </Row>
    </Column>
  );
}
