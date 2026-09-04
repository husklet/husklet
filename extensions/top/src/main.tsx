import { connect, render, Text, workspace, type InterfaceSourceMutation, type RenderHandle } from '@husklet/react';
import { Top } from './app.js';
import { selections } from './selection.js';
import { ContainerDetailsSource, ExecutionDetailsSource, ImageDetailsSource, NetworkDetailsSource, VolumeDetailsSource } from './model.js';

const providerSelections = selections();
let surface!: RenderHandle;
const send = (mutation: InterfaceSourceMutation) => surface.source(mutation);
const imageDetails = new ImageDetailsSource(send);
const containerDetails = new ContainerDetailsSource(send);
const executionDetails = new ExecutionDetailsSource(send);
const networkDetails = new NetworkDetailsSource(send);
const volumeDetails = new VolumeDetailsSource(send);
const session = await connect({
  onRows(request, channel) {
    const window = imageDetails.answer(request) ?? containerDetails.answer(request) ?? executionDetails.answer(request) ?? networkDetails.answer(request) ?? volumeDetails.answer(request);
    if (window) session.answer(channel, window);
  },
  onEvent(payload) {
    if (payload && typeof payload === 'object') providerSelections.publish(payload);
  },
});
surface = render(<Text label={'Loading workspace resources…'} />, session, { title: 'Top' });
await surface.ready;
surface.update(
  <Top api={workspace(session)} selections={providerSelections} containerDetails={containerDetails} executionDetails={executionDetails} imageDetails={imageDetails} networkDetails={networkDetails} volumeDetails={volumeDetails} />,
);
