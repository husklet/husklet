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

const session = await connect();
render(React.createElement(App), session, { title: 'React starter' });
