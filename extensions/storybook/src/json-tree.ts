// @ts-nocheck -- legacy story typing is migrated incrementally.
import React from 'react';
import { Badge, Column, Heading, InlineMessage, JsonTree, Row, Text } from '@husklet/react';

const { createElement: h, useMemo, useState } = React;

export const JSON_TREE_STORY = 'Bounded JSON tree';

export function JsonTreeStory() {
  const [activity, setActivity] = useState('Select or copy a value to inspect its callback.');
  const payload = useMemo(() => {
    const value = {
      status: 'ready',
      workspace: { name: 'development', features: ['terminal', 'extensions'], owner: null },
      metrics: { cpu: 0.42, healthy: true },
      long: 'bounded '.repeat(80),
      nested: { one: { two: { three: { four: 'depth-bound' } } } },
    };
    value.self = value;
    return value;
  }, []);
  return h(Column, { gap: 2, grow: true },
    h(Heading, { label: 'Bounded JSON tree', scale: 'title' }),
    h(Text, { label: 'Expand paths, filter nested values, and invoke select or copy callbacks without a web view.' }),
    h(Row, { gap: 1 }, h(Badge, { label: 'cycle-safe', tone: 'positive' }), h(Badge, { label: 'bounded', tone: 'warning' })),
    h(JsonTree, {
      value: payload, maxDepth: 4, maxNodes: 40, maxStringLength: 48, grow: true,
      onSelect: ({ path, type }) => setActivity(`Selected ${path} (${type})`),
      onCopy: ({ path, text }) => setActivity(`Copy requested for ${path}: ${text}`),
    }),
    h(InlineMessage, { label: activity }),
  );
}
