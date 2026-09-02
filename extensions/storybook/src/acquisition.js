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
  Spinner,
  Text,
} from '@husklet/react';

const { createElement: h, useState } = React;

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
  return h(
    Column,
    { gap: 3, grow: true },
    h(Heading, { key: 'title', label: 'Extension acquisition states', scale: 'title', wrap: true }),
    h(Text, {
      key: 'explanation',
      label: 'Acquisition is read-only until the ready state. Cancel exists only while work is pending; Retry exists only after failure.',
      wrap: true,
      color: 'text-dim',
    }),
    ...acquisitionStates.map((state) => h(AcquisitionState, { key: state.key, state, onAction: (label) => setEvent(`${label} invoked for ${state.key}.`) })),
    h(InlineMessage, { key: 'event', label: event, tone: 'neutral' }),
  );
}

function AcquisitionState({ state, onAction }) {
  const activity =
    state.activity === 'progress'
      ? h(Progress, { key: 'activity', fraction: state.fraction, tooltip: state.status })
      : state.activity === 'spinner'
        ? h(Spinner, { key: 'activity', busy: true, tooltip: state.status })
        : null;
  return h(
    Card,
    { label: state.title, tone: state.tone ?? 'neutral', variant: 'outline' },
    h(CardHeader, { key: 'header', label: state.title, detail: state.key }),
    h(
      CardContent,
      { key: 'content', gap: 2 },
      activity,
      h(InlineMessage, { key: 'status', label: state.status, tone: state.tone ?? 'neutral' }),
    ),
    h(
      CardActions,
      { key: 'actions', gap: 2 },
      h(Column, { gap: 2 }, ...state.actions.map((label) => h(Button, { key: label, label, onInvoke: () => onAction(label) }))),
    ),
  );
}
