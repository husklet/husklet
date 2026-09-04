// The entrypoint the host starts: connect, then render the playground.

import React from 'react';
import { connect, render } from '@husklet/react';
import type { InterfaceSourceMutation, RenderHandle, Session } from '@husklet/react';

import { Playground } from './app.js';
import { LargeRecordSource } from './large-table.js';
import { TimelineSource } from './event-stream.js';
import { KeyValueSource } from './key-value-inspector.js';
import { FileSource } from './file-browser.js';

type SourceSender = (_call: string, argument: { mutation: InterfaceSourceMutation }) => Promise<void>;
type SourceConstructor<T> = new (send: SourceSender) => T;

let session: Session;
let surface: RenderHandle;
const send: SourceSender = (_call, argument) => surface.source(argument.mutation);
const source = new (LargeRecordSource as unknown as SourceConstructor<LargeRecordSource>)(send);
const timeline = new (TimelineSource as unknown as SourceConstructor<TimelineSource>)(send);
const keyValues = new (KeyValueSource as unknown as SourceConstructor<KeyValueSource>)(send);
const files = new (FileSource as unknown as SourceConstructor<FileSource>)(send);
const PlaygroundComponent = Playground as React.ComponentType<{
  largeSource: LargeRecordSource;
  timelineSource: TimelineSource;
  keyValueSource: KeyValueSource;
  fileSource: FileSource;
  initialStory?: string;
}>;
session = await connect({
  onRows: (request, channel) => {
    const window = source.answer(request) ?? timeline.answer(request) ?? keyValues.answer(request) ?? files.answer(request);
    if (window) session.answer(channel, window);
  },
});
surface = render(React.createElement(PlaygroundComponent, {
  largeSource: source,
  timelineSource: timeline,
  keyValueSource: keyValues,
  fileSource: files,
  initialStory: process.env.HUSKLET_STORYBOOK_STORY,
}), session, { title: 'Storybook' });
setTimeout(() => void source.publish(), 0);
setTimeout(() => void timeline.publish(), 0);
setTimeout(() => void keyValues.publish(), 0);
setTimeout(() => void files.publish(), 0);
