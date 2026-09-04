import { connect, render, workspace } from '@husklet/react';
import { Extensions } from './extensions.js';

const session = await connect();
render(<Extensions api={workspace(session)} />, session, { title: 'Extensions' });
