import { bootstrapSurface, connect } from '@husklet/client';
import type { InterfaceSourceMutation } from '@husklet/react';
import { selections } from './selection.js';
import type {
  ContainerDetailsSource,
  ExecutionDetailsSource,
  ImageDetailsSource,
  ProcessTableSource,
  VolumeDetailsSource,
} from './model.js';
import { SECTIONS } from './overview.js';

const providerSelections = selections();
let surface: import('@husklet/react').RenderHandle;
let imageDetails: ImageDetailsSource | undefined;
let containerDetails: ContainerDetailsSource | undefined;
let executionDetails: ExecutionDetailsSource | undefined;
let volumeDetails: VolumeDetailsSource | undefined;
let processTable: ProcessTableSource | undefined;
const send = (mutation: InterfaceSourceMutation) => surface.source(mutation);
const session = await connect({
  onRows(request, channel) {
    const window =
      imageDetails?.answer(request) ??
      containerDetails?.answer(request) ??
      executionDetails?.answer(request) ??
      volumeDetails?.answer(request) ??
      processTable?.answer(request);
    session.answer(
      channel,
      window ?? {
        source: request.source,
        version: request.version,
        request: request.id,
        range: request.range,
        rows: [],
      },
    );
  },
  onEvent(payload) {
    if (payload && typeof payload === 'object') providerSelections.publish(payload);
  },
});
const bootstrap = await bootstrapSurface(session, {
  title: 'Top',
  label: 'Loading workspace resources…',
  primary: true,
});
const [{ render, Text, workspace }, { Top }, models] = await Promise.all([
  import('@husklet/react'),
  import('./app.js'),
  import('./model.js'),
]);
const fixture = ['populated', 'error', 'partial-processes'].includes(
  process.env.HUSKLET_TOP_FIXTURE ?? '',
)
  ? process.env.HUSKLET_TOP_FIXTURE
  : undefined;
const fixtureModule = fixture ? await import('./fixture.js') : null;
const api = workspace(session);
surface = render(<Text label={'Loading workspace resources…'} />, session, {
  title: 'Top',
  bootstrap,
});
await surface.ready;
imageDetails = new models.ImageDetailsSource(send);
containerDetails = new models.ContainerDetailsSource(send);
executionDetails = new models.ExecutionDetailsSource(send);
volumeDetails = new models.VolumeDetailsSource(send);
processTable = new models.ProcessTableSource(send);
surface.update(
  <Top
    api={fixtureModule ? fixtureModule.fixtureApi(api, fixture) : api}
    selections={providerSelections}
    containerDetails={containerDetails}
    executionDetails={executionDetails}
    imageDetails={imageDetails}
    processTable={processTable}
    volumeDetails={volumeDetails}
    initial={
      fixture === 'error'
        ? { ...fixtureModule?.populatedFixture, networks: undefined }
        : fixture === 'partial-processes'
          ? fixtureModule?.partialProcessFixture
          : fixtureModule?.populatedFixture
    }
    initialSection={
      SECTIONS.includes(process.env.HUSKLET_TOP_SECTION as (typeof SECTIONS)[number])
        ? (process.env.HUSKLET_TOP_SECTION as (typeof SECTIONS)[number])
        : undefined
    }
  />,
);
await surface.flush();
