import React, { useState } from 'react';
import { Button, Column, Heading, Text, connect, render } from '@husklet/react';

function App() {
  const [count, setCount] = useState(0);
  return React.createElement(
    Column,
    { gap: 2, pad: 4 },
    React.createElement(Heading, { label: 'React starter', scale: 'title' }),
    React.createElement(Text, { label: `Clicked ${count} times` }),
    React.createElement(Button, {
      label: 'Increment',
      tone: 'accent',
      onInvoke: () => setCount((current) => current + 1),
    }),
  );
}

let session;
let stopping = false;
let connected = false;
const report = (kind, error) => process.stderr.write(`react-starter: ${kind}: ${error instanceof Error ? error.message : String(error)}\n`);
const stop = () => {
  if (stopping) return;
  stopping = true;
  session?.close();
};

try {
  session = await connect({
    onClose: (error) => {
      if (!connected || stopping) return;
      stopping = true;
      process.exitCode = 1;
      report('host connection ended', error);
    },
  });
  connected = true;
  process.once('SIGINT', stop);
  process.once('SIGTERM', stop);
  const surface = render(React.createElement(App), session, { title: 'React starter' });
  await surface.ready;
} catch (error) {
  if (!stopping) {
    stopping = true;
    process.exitCode = 1;
    report('startup failed', error);
  }
  session?.close();
}
