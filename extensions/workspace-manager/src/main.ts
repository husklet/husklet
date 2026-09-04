// @ts-nocheck -- entrypoint typing follows the model migration.
import React from 'react';
import { connect, render, workspace } from '@husklet/react';
import { WorkspaceManager } from './app.js';
import { selections } from './selection.js';
import { ContainerDetailsSource, ExecutionDetailsSource, ImageDetailsSource, NetworkDetailsSource, VolumeDetailsSource } from './model.js';

const providerSelections = selections();
let surface;
const imageDetails = new ImageDetailsSource((mutation) => surface.source(mutation));
const containerDetails = new ContainerDetailsSource((mutation) => surface.source(mutation));
const executionDetails = new ExecutionDetailsSource((mutation) => surface.source(mutation));
const networkDetails = new NetworkDetailsSource((mutation) => surface.source(mutation));
const volumeDetails = new VolumeDetailsSource((mutation) => surface.source(mutation));
const session = await connect({
  onRows(request, channel) {
    const window = imageDetails.answer(request) ?? containerDetails.answer(request) ?? executionDetails.answer(request) ?? networkDetails.answer(request) ?? volumeDetails.answer(request);
    if (window) session.answer(channel, window);
  },
  onEvent(payload) {
    if (payload && typeof payload === 'object') providerSelections.publish(payload);
  },
});
surface = render(React.createElement(WorkspaceManager, { api: workspace(session), selections: providerSelections, containerDetails, executionDetails, imageDetails, networkDetails, volumeDetails }), session, {
  title: 'Resources',
});
