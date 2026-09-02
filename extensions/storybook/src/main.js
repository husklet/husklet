// The entrypoint the host starts: connect, then render the playground.

import React from 'react';
import { connect, render } from '@husklet/react';

import { Playground } from './app.js';
import { LargeRecordSource } from './large-table.js';

let session;
const source = new LargeRecordSource((call, argument) => session.call(call, argument));
session = await connect({
  onRows: (request, channel) => {
    const window = source.answer(request);
    if (window) session.answer(channel, window);
  },
});
render(React.createElement(Playground, { largeSource: source }), session, { title: 'Storybook' });
setTimeout(() => void source.publish(), 0);
