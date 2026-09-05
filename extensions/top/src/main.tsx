import { bootstrapSurface, connect } from '@husklet/client';
import type { InterfaceSourceMutation } from '@husklet/react';
import { selections } from './selection.js';
import type { ContainerDetailsSource, ExecutionDetailsSource, ImageDetailsSource, NetworkDetailsSource, VolumeDetailsSource } from './model.js';

const providerSelections = selections();
let surface: import('@husklet/react').RenderHandle;
let imageDetails: ImageDetailsSource | undefined;
let containerDetails: ContainerDetailsSource | undefined;
let executionDetails: ExecutionDetailsSource | undefined;
let networkDetails: NetworkDetailsSource | undefined;
let volumeDetails: VolumeDetailsSource | undefined;
const send = (mutation: InterfaceSourceMutation) => surface.source(mutation);
const session = await connect({
  onRows(request, channel) {
    const window = imageDetails?.answer(request) ?? containerDetails?.answer(request) ?? executionDetails?.answer(request) ?? networkDetails?.answer(request) ?? volumeDetails?.answer(request);
    if (window) session.answer(channel, window);
  },
  onEvent(payload) {
    if (payload && typeof payload === 'object') providerSelections.publish(payload);
  },
});
const bootstrap = await bootstrapSurface(session, { title: 'Top', label: 'Loading workspace resources…', primary: true });
const [{ render, Text, workspace }, { Top }, models] = await Promise.all([
  import('@husklet/react'),
  import('./app.js'),
  import('./model.js'),
]);
surface = render(<Text label={'Loading workspace resources…'} />, session, { title: 'Top', bootstrap });
await surface.ready;
imageDetails = new models.ImageDetailsSource(send);
containerDetails = new models.ContainerDetailsSource(send);
executionDetails = new models.ExecutionDetailsSource(send);
networkDetails = new models.NetworkDetailsSource(send);
volumeDetails = new models.VolumeDetailsSource(send);
surface.update(
  <Top api={workspace(session)} selections={providerSelections} containerDetails={containerDetails} executionDetails={executionDetails} imageDetails={imageDetails} networkDetails={networkDetails} volumeDetails={volumeDetails} />,
);
await surface.flush();
