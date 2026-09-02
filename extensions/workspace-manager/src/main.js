import React from 'react';
import { connect, render, workspace } from '@husklet/react';
import { WorkspaceManager } from './app.js';
import { selections } from './selection.js';

const providerSelections = selections();
const session = await connect({
  onEvent(payload) {
    if (payload && typeof payload === 'object') providerSelections.publish(payload);
  },
});
render(React.createElement(WorkspaceManager, { api: workspace(session), selections: providerSelections }), session, {
  title: 'Resources',
});
