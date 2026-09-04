// @ts-nocheck -- legacy story typing is migrated incrementally.
import React, { useState } from 'react';
import { Button, Card, CardContent, Column, Heading, InlineMessage, Progress, Row, Select, Text } from '@husklet/react';

const { createElement: h } = React;

export const IMAGE_PULL_STORY = 'Multi-platform image pull';
export const LAYER_LIMIT = 12;
export const REFERENCE_LIMIT = 256;
export const STATUS_LIMIT = 256;
export const PLATFORMS = Object.freeze(['linux/amd64', 'linux/arm64']);
const DIGEST = /^sha256:[0-9a-f]{64}$/;
const clean = (value, limit) => String(value ?? '').replace(/[\r\n\t]/g, ' ').slice(0, limit);
const bytes = (value) => Number.isSafeInteger(value) && value >= 0 ? value : 0;

export function boundedPull(pull) {
  const layers = (pull.layers ?? []).slice(0, LAYER_LIMIT).map((layer) => {
    const total = bytes(layer.total);
    return { id: clean(layer.id, 80), current: Math.min(bytes(layer.current), total), total };
  }).filter(({ id, total }) => id && total > 0);
  return {
    job: /^[1-9][0-9]{0,19}$/.test(pull.job ?? '') ? pull.job : '',
    reference: clean(pull.reference, REFERENCE_LIMIT),
    platform: PLATFORMS.includes(pull.platform) ? pull.platform : PLATFORMS[0],
    state: ['resolving', 'pulling', 'complete', 'cancelled', 'failed'].includes(pull.state) ? pull.state : 'failed',
    digest: DIGEST.test(pull.digest ?? '') ? pull.digest : '',
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
  const choosePlatform = (platform) => {
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

  return h(Column, { gap: 2, grow: true },
    h(Heading, { label: 'Multi-platform image pull', scale: 'title' }),
    h(Text, { label: 'One explicit reference resolves to one workspace architecture. Progress, cancellation, errors, and the final digest remain tied to job 42.', wrap: true }),
    h(Select, { value: pull.platform, choices: PLATFORMS.map((platform) => ({ value: platform, label: platform })), onChange: ({ value }) => choosePlatform(PLATFORMS.includes(value) ? value : PLATFORMS[0]) }),
    h(Card, { label: pull.reference, variant: 'outline' }, h(CardContent, { gap: 2 },
      h(Text, { label: `${pull.state} · ${pull.platform} · job ${pull.job}`, monospace: true }),
      ...pull.layers.map((layer) => h(Column, { key: layer.id, gap: 1 },
        h(Text, { label: `${layer.id} · ${layer.current}/${layer.total} bytes`, monospace: true }),
        h(Progress, { fraction: layer.current / layer.total }))),
      ...(pull.digest ? [h(Text, { key: 'digest', label: pull.digest, monospace: true, wrap: true })] : []),
      ...(pull.error ? [h(InlineMessage, { key: 'error', label: pull.error, tone: 'danger' })] : []))),
    h(Row, { gap: 2, wrap: true },
      ...(pull.state === 'pulling' ? [
        h(Button, { key: 'advance', label: 'Finish pull', onInvoke: advance }),
        h(Button, { key: 'cancel', label: 'Cancel pull', onInvoke: cancel }),
        h(Button, { key: 'fail', label: 'Simulate registry error', onInvoke: fail }),
      ] : [h(Button, { key: 'retry', label: 'Retry pull', onInvoke: () => choosePlatform(pull.platform) })])),
    h(InlineMessage, { label: message, tone: pull.state === 'failed' ? 'danger' : 'neutral' }));
}
