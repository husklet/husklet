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
  InlineMessage,
  Progress,
  Row,
  Select,
  Spinner,
  Text,
} from '@husklet/react';

const { useState } = React;

export const ACQUISITION_STORY = 'Extension acquisition';

type Tone = 'neutral' | 'positive' | 'danger';
type Activity = 'spinner' | 'progress';
export interface AcquisitionStateModel {
  key: string;
  title: string;
  status: string;
  activity?: Activity;
  fraction?: number;
  tone?: Tone;
  actions: readonly string[];
}

export const acquisitionStates: readonly AcquisitionStateModel[] = [
  {
    key: 'checking',
    title: 'Checking',
    status: 'checking local images',
    activity: 'spinner',
    actions: ['Cancel download'],
  },
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
  {
    key: 'manifest',
    title: 'Reading manifest',
    status: 'reading extension manifest',
    activity: 'spinner',
    actions: ['Cancel download'],
  },
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
    <Column gap={3} width="fill">
      <Text
        key={'explanation'}
        label={
          'Acquisition is read-only until the ready state. Cancel exists only while work is pending; Retry exists only after failure.'
        }
        wrap={true}
        color={'text-dim'}
      />
      <Select
        key={'state'}
        value={state.key}
        width={{ maximum: { chars: 32 } }}
        choices={acquisitionStates.map(({ key, title }) => ({ value: key, label: title }))}
        onChange={({ value }) => setSelected(String(value ?? acquisitionStates[0].key))}
      />
      <AcquisitionState
        key={state.key}
        state={state}
        onAction={(label) => setEvent(`${label} invoked for ${state.key}.`)}
      />
      <InlineMessage key={'event'} label={event} tone={'neutral'} />
    </Column>
  );
}

function AcquisitionState({
  state,
  onAction,
}: {
  state: AcquisitionStateModel;
  onAction: (label: string) => void;
}) {
  const activity =
    state.activity === 'progress' ? (
      <Progress key={'activity'} fraction={state.fraction} tooltip={state.status} />
    ) : state.activity === 'spinner' ? (
      <Spinner key={'activity'} busy={true} tooltip={state.status} />
    ) : null;
  return (
    <Card width="fill" label={state.title} tone={state.tone ?? 'neutral'} variant={'outline'}>
      <CardHeader key={'header'} label={state.title} detail={state.key} />
      <CardContent key={'content'} gap={2}>
        <Row gap={2} align="center" width="fill">
          {activity}
          <InlineMessage key={'status'} label={state.status} tone={state.tone ?? 'neutral'} />
        </Row>
      </CardContent>
      <CardActions key={'actions'} gap={2}>
        <Row gap={2} wrap>
          {state.actions.map((label) => (
            <Button
              key={label}
              label={label}
              size="small"
              variant={label === 'Install' ? 'filled' : 'outline'}
              tone={label === 'Retry' ? 'danger' : label === 'Install' ? 'accent' : 'neutral'}
              onInvoke={() => onAction(label)}
            />
          ))}
        </Row>
      </CardActions>
    </Card>
  );
}
