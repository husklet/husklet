import React from 'react';
import { Column, FlameGraph, Heading, InlineMessage, Text } from '@husklet/react';


export const PROFILE_STORY = 'Inspect sampled profile';
export const FRAME_LIMIT = 64;
type ProfileFrame = { label: string; samples: number };

export function boundedFrames(frames: readonly unknown[]): string {
  return frames
    .filter((frame): frame is ProfileFrame => {
      if (frame === null || typeof frame !== 'object') return false;
      const { label, samples } = frame as Record<string, unknown>;
      return typeof label === 'string' && Boolean(label.trim()) && Number.isSafeInteger(samples) && Number(samples) > 0;
    })
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
