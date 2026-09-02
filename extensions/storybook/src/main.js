// The entrypoint the host starts: connect, then render the playground.

import React from 'react';
import { connect, render } from '@husklet/react';

import { Playground } from './app.js';
import { LargeRecordSource } from './large-table.js';
import { TimelineSource } from './event-stream.js';
import { KeyValueSource } from './key-value-inspector.js';
import { FileSource } from './file-browser.js';

let session;
let surface;
const source = new LargeRecordSource((_call, argument) => surface.source(argument.mutation));
const timeline = new TimelineSource((_call, argument) => surface.source(argument.mutation));
const keyValues = new KeyValueSource((_call, argument) => surface.source(argument.mutation));
const files = new FileSource((_call, argument) => surface.source(argument.mutation));
session = await connect({
  onRows: (request, channel) => {
    const window = source.answer(request) ?? timeline.answer(request) ?? keyValues.answer(request) ?? files.answer(request);
    if (window) session.answer(channel, window);
  },
});
surface = render(React.createElement(Playground, {
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
