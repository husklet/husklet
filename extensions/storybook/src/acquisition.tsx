// @ts-nocheck -- legacy story typing is migrated incrementally.
// The complete end-user image-acquisition state model, shown together so
// progress wording and available actions can be reviewed without a registry.

import React from 'react';
import {
  Button,
  Card,
  CardActions,
  CardContent,
  CardHeader,
  Column,
  Heading,
  InlineMessage,
  Progress,
  Select,
  Spinner,
  Text,
} from '@husklet/react';

const { useState } = React;

export const ACQUISITION_STORY = 'Extension acquisition';

export const acquisitionStates = [
  { key: 'checking', title: 'Checking', status: 'checking local images', activity: 'spinner', actions: ['Cancel download'] },
  {
    key: 'pulling-indeterminate',
    title: 'Downloading — total unknown',
    status: 'Pulling from team/tool · layer 3; progress unavailable',
    activity: 'spinner',
    actions: ['Cancel download'],
  },
  {
    key: 'pulling-determinate',
    title: 'Downloading — measured',
    status: 'Downloading · layer 4; 25%; 25 of 100 bytes',
    activity: 'progress',
    fraction: 0.25,
    actions: ['Cancel download'],
  },
  { key: 'manifest', title: 'Reading manifest', status: 'reading extension manifest', activity: 'spinner', actions: ['Cancel download'] },
  {
    key: 'failure',
    title: 'Failed',
    status: 'registry request failed; the installed extension is unchanged',
    tone: 'danger',
    actions: ['Retry'],
  },
  {
    key: 'ready',
    title: 'Ready for consent',
    status: 'team-tool 1.2.3 at sha256:4f… asks for the capabilities above',
    tone: 'positive',
    actions: ['Install', 'Cancel'],
  },
];

export function AcquisitionProgressStory() {
  const [event, setEvent] = useState('No acquisition action invoked.');
  const [selected, setSelected] = useState(acquisitionStates[0].key);
  const state = acquisitionStates.find(({ key }) => key === selected) ?? acquisitionStates[0];
  return (
    <Column gap={3} grow={true}>
      <Heading
        key={'title'}
        label={'Extension acquisition states'}
        scale={'title'}
        wrap={true} />
      <Text
        key={'explanation'}
        label={'Acquisition is read-only until the ready state. Cancel exists only while work is pending; Retry exists only after failure.'}
        wrap={true}
        color={'text-dim'} />
      <Select
        key={'state'}
        value={state.key}
        choices={acquisitionStates.map(({ key, title }) => ({ value: key, label: title }))}
        onChange={({ value }) => setSelected(String(value ?? acquisitionStates[0].key))} />
      <AcquisitionState
        key={state.key}
        state={state}
        onAction={(label) => setEvent(`${label} invoked for ${state.key}.`)} />
      <InlineMessage key={'event'} label={event} tone={'neutral'} />
    </Column>
  );
}

function AcquisitionState({ state, onAction }) {
  const activity =
    state.activity === 'progress'
      ? <Progress key={'activity'} fraction={state.fraction} tooltip={state.status} />
      : state.activity === 'spinner'
        ? <Spinner key={'activity'} busy={true} tooltip={state.status} />
        : null;
  return (
    <Card label={state.title} tone={state.tone ?? 'neutral'} variant={'outline'}>
      <CardHeader key={'header'} label={state.title} detail={state.key} />
      <CardContent key={'content'} gap={2}>
        {activity}
        <InlineMessage key={'status'} label={state.status} tone={state.tone ?? 'neutral'} />
      </CardContent>
      <CardActions key={'actions'} gap={2}>
        <Column gap={2}>
          {state.actions.map((label) => <Button key={label} label={label} onInvoke={() => onAction(label)} />)}
        </Column>
      </CardActions>
    </Card>
  );
}
