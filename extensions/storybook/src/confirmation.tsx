import React, { useState } from 'react';
import { Column, ConfirmAction, Heading, InlineMessage, Row, Text } from '@husklet/react';

export const CONFIRMATION_STORY = 'ConfirmAction';

/** A complete destructive flow: reveal authority, confirm separately, and report completion. */
export function ConfirmationStory() {
  const [removed, setRemoved] = useState(false);
  return (
    <Column gap={3}>
      <Heading label={'Destructive flow'} scale={'title'} />
      <Text
        label={'A destructive action reveals its exact authority before it can run.'}
        wrap={true}
      />
      {removed ? (
        <InlineMessage label={'Volume cache was removed.'} tone={'positive'} />
      ) : (
        <ConfirmAction
          authorityKey={'volume:cache:generation-7'}
          label={'Remove volume'}
          confirmLabel={'Confirm removal'}
          question={'Remove volume cache generation 7? This cannot be undone.'}
          onConfirm={async () => setRemoved(true)}
        />
      )}
      <Heading label={'Sizes'} scale={'title'} />
      <Row gap={2} wrap={true}>
        <ConfirmAction
          authorityKey={'small-example'}
          label={'Small'}
          confirmLabel={'Confirm small'}
          question={'Confirm the small action?'}
          size={'small'}
          onConfirm={() => {}}
        />
        <ConfirmAction
          authorityKey={'medium-example'}
          label={'Medium'}
          confirmLabel={'Confirm medium'}
          question={'Confirm the medium action?'}
          size={'medium'}
          onConfirm={() => {}}
        />
        <ConfirmAction
          authorityKey={'large-example'}
          label={'Large'}
          confirmLabel={'Confirm large'}
          question={'Confirm the large action?'}
          size={'large'}
          onConfirm={() => {}}
        />
      </Row>
      <Text
        label={
          'Use small in compact management rows, medium by default, and large only for touch-forward layouts.'
        }
        color={'text-dim'}
        wrap={true}
      />
    </Column>
  );
}
