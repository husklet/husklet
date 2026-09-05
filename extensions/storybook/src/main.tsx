// Paint through the framework-neutral client before loading the component catalogue.

import { bootstrapSurface, connect } from '@husklet/client';
import type { InterfaceSourceMutation, RenderHandle } from '@husklet/react';

type RowSource = { answer(request: unknown): unknown; publish(): Promise<unknown> };
type SourceSender = (_call: string, argument: { mutation: InterfaceSourceMutation }) => Promise<void>;
type SourceConstructor = new (send: SourceSender) => RowSource;

let surface: RenderHandle;
let sources: RowSource[] = [];
const session = await connect({
  onRows(request, channel) {
    const window = sources.map((source) => source.answer(request)).find(Boolean);
    if (window) session.answer(channel, window);
  },
});
const bootstrap = await bootstrapSurface(session, {
  title: 'Components',
  label: 'Loading component playground…',
  primary: true,
});
const [React, react, app, large, events, keyValues, files] = await Promise.all([
  import('react').then((module) => module.default),
  import('@husklet/react'),
  import('./app.js'),
  import('./large-table.js'),
  import('./event-stream.js'),
  import('./key-value-inspector.js'),
  import('./file-browser.js'),
]);
const send: SourceSender = (_call, argument) => surface.source(argument.mutation);
sources = [large.LargeRecordSource, events.TimelineSource, keyValues.KeyValueSource, files.FileSource]
  .map((Source) => new (Source as unknown as SourceConstructor)(send));
const [source, timeline, keyValueSource, fileSource] = sources;
const Playground = app.Playground as unknown as React.ComponentType<Record<string, unknown>>;
surface = react.render(React.createElement(Playground, {
  largeSource: source,
  timelineSource: timeline,
  keyValueSource,
  fileSource,
  initialStory: process.env.HUSKLET_STORYBOOK_STORY,
}), session, { title: 'Components', bootstrap });
await surface.flush();
for (const sourceModel of sources) void sourceModel.publish();
