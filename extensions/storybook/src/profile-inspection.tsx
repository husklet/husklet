// @ts-nocheck -- legacy story typing is migrated incrementally.
import React from 'react';
import { Column, FlameGraph, Heading, InlineMessage, Text } from '@husklet/react';


export const PROFILE_STORY = 'Inspect sampled profile';
export const FRAME_LIMIT = 64;

export function boundedFrames(frames) {
  return frames
    .filter(({ label, samples }) => typeof label === 'string' && label.trim() && Number.isSafeInteger(samples) && samples > 0)
    .slice(0, FRAME_LIMIT)
    .map(({ label, samples }) => `${samples}\t${label.replace(/[\t\r\n]/g, ' ')}`)
    .join('\n');
}

export function ProfileInspectionStory() {
  const value = boundedFrames([
    { label: 'compiler::parse', samples: 120 },
    { label: 'compiler::type_check', samples: 86 },
    { label: 'compiler::optimize', samples: 54 },
    { label: 'compiler::emit', samples: 31 },
  ]);
  return (
    <Column gap={2} grow={true}>
      <Heading label={'Sampled CPU profile'} scale={'title'} />
      <Text
        label={'Frame labels are selectable; bar length is proportional to observed samples.'} />
      <FlameGraph value={value} tone={'accent'} grow={true} />
      <InlineMessage label={`Showing 4 of at most ${FRAME_LIMIT} frames`} />
    </Column>
  );
}
