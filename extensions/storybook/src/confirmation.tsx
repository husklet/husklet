import React, { useState } from 'react';
import { Column, ConfirmAction, Heading, InlineMessage, Text } from '@husklet/react';


export const CONFIRMATION_STORY = 'Safe destructive confirmation';

/** A complete destructive flow: reveal authority, confirm separately, and report completion. */
export function ConfirmationStory() {
  const [removed, setRemoved] = useState(false);
  return (
    <Column gap={2}>
      <Heading label={'Remove cache volume'} scale={'title'} />
      <Text
        label={'The immutable volume generation is shown before any destructive request.'}
        wrap={true} />
      {removed
        ? <InlineMessage label={'Volume cache was removed.'} tone={'positive'} />
        : <ConfirmAction
        authorityKey={'volume:cache:generation-7'}
        label={'Remove volume'}
        confirmLabel={'Confirm removal'}
        question={'Remove volume cache generation 7? This cannot be undone.'}
        onConfirm={async () => setRemoved(true)} />}
    </Column>
  );
}
