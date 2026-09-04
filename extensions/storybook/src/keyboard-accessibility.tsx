// @ts-nocheck -- legacy story typing is migrated incrementally.
import React from 'react';
import {
  Button,
  Column,
  Entry,
  FormControl,
  FormHelperText,
  FormLabel,
  Heading,
  InlineMessage,
  Row,
  Text,
} from '@husklet/react';

const { useRef, useState } = React;

export const KEYBOARD_STORY = 'Keyboard and semantic actions';
export const EVENT_LIMIT = 6;

/** Keyboard-first destructive flow with validation and a bounded visible audit trail. */
export function KeyboardAccessibilityStory() {
  const [name, setName] = useState('');
  const [attempted, setAttempted] = useState(false);
  const [confirming, setConfirming] = useState(false);
  const [events, setEvents] = useState([]);
  const sequence = useRef(0);
  const invalid = attempted && name.trim().length < 3;
  const record = (label) => {
    const event = `#${++sequence.current} ${label}`;
    setEvents((current) => [...current, event].slice(-EVENT_LIMIT));
  };
  const review = () => {
    setAttempted(true);
    if (name.trim().length < 3) {
      record('validation failed');
      return;
    }
    setConfirming(true);
    record('confirmation opened');
  };

  return (
    <Column gap={3} width={{ maximum: { chars: 62 } }}>
      <Heading label={'Keyboard-safe extension removal'} scale={'title'} wrap={true} />
      <Text
        label={'Tab through the enabled controls. Focus events and actions appear in the bounded history below.'}
        color={'text-dim'}
        wrap={true} />
      <FormControl gap={1}>
        <FormLabel label={'Extension name'} />
        <Entry
          value={name}
          placeholder={'storybook'}
          tone={invalid ? 'danger' : 'neutral'}
          onFocus={() => record('focused Extension name')}
          onChange={(event) => {
            setName(String(event.value ?? ''));
            setAttempted(false);
            record('changed Extension name');
          }}
          onSubmit={review} />
        <FormHelperText
          label={invalid ? 'Enter at least 3 characters before continuing.' : 'Confirmation is a separate step.'}
          tone={invalid ? 'danger' : 'neutral'} />
      </FormControl>
      <Row gap={2} justify={'end'} wrap={true}>
        <Button
          label={'Unavailable'}
          enabled={false}
          tooltip={'Disabled controls are skipped by keyboard traversal.'}
          onFocus={() => record('ERROR disabled control focused')} />
        <Button
          label={'Review removal'}
          tone={'accent'}
          onFocus={() => record('focused Review removal')}
          onInvoke={review} />
      </Row>
      {invalid
        ? [<InlineMessage key={'validation'}
        label={'Resolve the validation error before confirmation.'}
        tone={'danger'} />]
        : []}
      {confirming
        ? [
            <InlineMessage key={'confirmation'}
              label={`Remove ${name.trim()}? This confirmation is intentionally explicit.`}
              tone={'warning'} />,
            <Row key={'confirmation-actions'} gap={2} justify={'end'} wrap={true}>
              <Button
                label={'Cancel'}
                onFocus={() => record('focused Cancel')}
                onInvoke={() => {
                  setConfirming(false);
                  record('confirmation cancelled');
                }} />
              <Button
                label={'Confirm removal'}
                destructive={true}
                tone={'danger'}
                onFocus={() => record('focused Confirm removal')}
                onInvoke={() => {
                  setConfirming(false);
                  record('removal confirmed');
                }} />
            </Row>,
          ]
        : []}
      <Text
        label={`Event history (${events.length}/${EVENT_LIMIT})`}
        color={'text-dim'} />
      {events.length === 0
        ? [<InlineMessage key={'empty-events'} label={'No keyboard or semantic events yet.'} tone={'neutral'} />]
        : events.map((event) => <InlineMessage key={event} label={event} tone={'positive'} />)}
    </Column>
  );
}
