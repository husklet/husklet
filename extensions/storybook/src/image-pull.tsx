import React, { useState } from 'react';
import { Button, Card, CardContent, CodeView, Column, Heading, InlineMessage, Progress, Row, Select, Text } from '@husklet/react';


export const IMAGE_PULL_STORY = 'Multi-platform image pull';
export const LAYER_LIMIT = 12;
export const REFERENCE_LIMIT = 256;
export const STATUS_LIMIT = 256;
export const PLATFORMS = Object.freeze(['linux/amd64', 'linux/arm64'] as const);
export type Platform = typeof PLATFORMS[number];
export type PullState = 'resolving' | 'pulling' | 'complete' | 'cancelled' | 'failed';
export interface PullLayerInput { id?: unknown; current?: unknown; total?: unknown; }
export interface PullInput {
  job?: unknown;
  reference?: unknown;
  platform?: unknown;
  state?: unknown;
  digest?: unknown;
  error?: unknown;
  layers?: readonly PullLayerInput[];
}
export interface BoundedPull {
  job: string;
  reference: string;
  platform: Platform;
  state: PullState;
  digest: string;
  error: string;
  layers: Array<{ id: string; current: number; total: number }>;
}
const DIGEST = /^sha256:[0-9a-f]{64}$/;
const clean = (value: unknown, limit: number) => String(value ?? '').replace(/[\r\n\t]/g, ' ').slice(0, limit);
const bytes = (value: unknown) => typeof value === 'number' && Number.isSafeInteger(value) && value >= 0 ? value : 0;

export function boundedPull(pull: PullInput): BoundedPull {
  const layers = (pull.layers ?? []).slice(0, LAYER_LIMIT).map((layer) => {
    const total = bytes(layer.total);
    return { id: clean(layer.id, 80), current: Math.min(bytes(layer.current), total), total };
  }).filter(({ id, total }) => id && total > 0);
  return {
    job: /^[1-9][0-9]{0,19}$/.test(String(pull.job ?? '')) ? String(pull.job) : '',
    reference: clean(pull.reference, REFERENCE_LIMIT),
    platform: PLATFORMS.includes(pull.platform as Platform) ? pull.platform as Platform : PLATFORMS[0],
    state: (['resolving', 'pulling', 'complete', 'cancelled', 'failed'].includes(String(pull.state))
      ? pull.state : 'failed') as PullState,
    digest: DIGEST.test(String(pull.digest ?? '')) ? String(pull.digest) : '',
    error: clean(pull.error, STATUS_LIMIT),
    layers,
  };
}

const digest = `sha256:${'4f'.repeat(32)}`;
const initial = boundedPull({
  job: '42', reference: 'ghcr.io/team/api:1.8.2', platform: PLATFORMS[0], state: 'pulling', digest: '', error: '',
  layers: [
    { id: 'manifest', current: 2_048, total: 2_048 },
    { id: 'runtime', current: 18_874_368, total: 31_457_280 },
    { id: 'application', current: 3_145_728, total: 12_582_912 },
  ],
});

export function ImagePullStory() {
  const [pull, setPull] = useState(initial);
  const [message, setMessage] = useState('Job 42 is pulling a manifest-selected platform with bounded layer progress.');
  const choosePlatform = (platform: Platform) => {
    setPull({ ...initial, platform });
    setMessage(`Resolved ${initial.reference} for ${platform}; no other platform is downloaded.`);
  };
  const advance = () => {
    const layers = pull.layers.map((layer) => ({ ...layer, current: layer.total }));
    setPull({ ...pull, layers, state: 'complete', digest });
    setMessage(`Verified immutable ${digest} for ${pull.platform}.`);
  };
  const cancel = () => {
    setPull({ ...pull, state: 'cancelled' });
    setMessage(`Cancelled image-pull job ${pull.job}; the existing local image is unchanged.`);
  };
  const fail = () => {
    setPull({ ...pull, state: 'failed', error: 'registry authorization expired; no partial image became visible' });
    setMessage('Pull failed safely; retry starts a new bounded observation cycle.');
  };

  return (
    <Column gap={2} grow={true}>
      <Heading label={'Multi-platform image pull'} scale={'title'} />
      <Text
        label={'One explicit reference resolves to one workspace architecture. Progress, cancellation, errors, and the final digest remain tied to job 42.'}
        wrap={true} />
      <Select
        value={pull.platform}
        choices={PLATFORMS.map((platform) => ({ value: platform, label: platform }))}
        onChange={({ value }) => choosePlatform(PLATFORMS.includes(value as Platform) ? value as Platform : PLATFORMS[0])} />
      <Card label={pull.reference} variant={'outline'}>
        <CardContent gap={2}>
          <CodeView value={`${pull.state} · ${pull.platform} · job ${pull.job}`} monospace={true} />
          {pull.layers.map((layer) => <Column key={layer.id} gap={1}>
            <CodeView value={`${layer.id} · ${layer.current}/${layer.total} bytes`} monospace={true} />
            <Progress fraction={layer.current / layer.total} />
          </Column>)}
          {pull.digest ? [<CodeView key={'digest'} value={pull.digest} monospace={true} />] : []}
          {pull.error ? [<InlineMessage key={'error'} label={pull.error} tone={'danger'} />] : []}
        </CardContent>
      </Card>
      <Row gap={2} wrap={true}>
        {pull.state === 'pulling' ? [
          <Button key={'advance'} label={'Finish pull'} onInvoke={advance} />,
          <Button key={'cancel'} label={'Cancel pull'} onInvoke={cancel} />,
          <Button key={'fail'} label={'Simulate registry error'} onInvoke={fail} />,
        ] : [<Button
          key={'retry'}
          label={'Retry pull'}
          onInvoke={() => choosePlatform(pull.platform)} />]}
      </Row>
      <InlineMessage label={message} tone={pull.state === 'failed' ? 'danger' : 'neutral'} />
    </Column>
  );
}
