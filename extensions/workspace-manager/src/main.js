import React from 'react';
import { connect, render, workspace } from '@husklet/react';
import { WorkspaceManager } from './app.js';
import { selections } from './selection.js';
import { ImageDetailsSource } from './model.js';

const providerSelections = selections();
let surface;
const imageDetails = new ImageDetailsSource((mutation) => surface.source(mutation));
const session = await connect({
  onRows(request, channel) {
    const window = imageDetails.answer(request);
    if (window) session.answer(channel, window);
  },
  onEvent(payload) {
    if (payload && typeof payload === 'object') providerSelections.publish(payload);
  },
});
surface = render(React.createElement(WorkspaceManager, { api: workspace(session), selections: providerSelections, imageDetails }), session, {
  title: 'Resources',
});
