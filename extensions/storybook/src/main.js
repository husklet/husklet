// The entrypoint the host starts: connect, then render the playground.

import React from 'react';
import { connect, render } from '@husklet/react';

import { Playground } from './app.js';

const session = await connect();
render(React.createElement(Playground), session, { title: 'Storybook' });
