import { connect, render, workspace } from '@husklet/react';
import { Workspace } from './workspace.js';

const session = await connect();
render(<Workspace api={workspace(session)} />, session, { title: 'Workspace' });
