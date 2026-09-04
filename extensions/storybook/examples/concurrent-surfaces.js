// Developer example: two independent React roots owned by one extension
// session. This is deliberately separate from src/main.ts so Storybook remains
// the canonical component catalogue rather than opening surprise panes.

import React, { useState } from 'react';
import { Button, Column, Heading, Row, Text, render } from '@husklet/react';

function Counter({ name, close }) {
  const [count, setCount] = useState(0);
  return React.createElement(
    Column,
    { gap: 3, pad: 4 },
    React.createElement(Heading, { label: name }),
    React.createElement(Text, { label: `Independent count: ${count}` }),
    React.createElement(
      Row,
      { gap: 2 },
      React.createElement(Button, { label: `Increment ${name}`, onInvoke: () => setCount((value) => value + 1) }),
      close && React.createElement(Button, { label: `Close ${name}`, onInvoke: () => void close() }),
    ),
  );
}

/**
 * Opens one tab and one sibling split from the same extension session.
 * Each handle owns a reconciler root, event route, frame sequence, and slot.
 */
export async function mountConcurrentSurfaces(session) {
  let overview;
  overview = render(React.createElement(Counter, { name: 'Overview', close: () => overview.close() }), session, {
    title: 'Concurrent surfaces',
  });
  const overviewSlot = await overview.ready;
  const details = render(React.createElement(Counter, { name: 'Details' }), session, {
    split: { slot: overviewSlot, division: 'beside' },
  });
  await details.ready;
  // overview.close() withdraws only overviewSlot; details remains mounted and
  // continues receiving its independently addressed events.
  return { overview, details };
}
