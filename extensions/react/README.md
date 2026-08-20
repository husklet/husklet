# @husklet/react

Write a workspace interface in React; the host renders it as native GTK widgets.
There is no DOM and no web view: components are the host's own component
library, and a React commit becomes one atomic frame of patches on a unix
socket.

## Using it

```dockerfile
FROM husklet/react:latest
COPY . /app
CMD ["node", "/app/main.js"]
LABEL husklet.extension.protocol="1"
LABEL husklet.extension.manifest="{...}"
```

```jsx
import { connect, render, Column, Button, Text } from '@husklet/react';

function App() {
  const [count, setCount] = React.useState(0);
  return (
    <Column gap={2} pad={4}>
      <Text scale="title">Clicked {count} times</Text>
      <Button label="Go" tone="accent" onInvoke={() => setCount(count + 1)} />
    </Column>
  );
}

const session = await connect();          // reads HUSKLET_EXTENSION_SOCKET
render(<App />, session, { title: 'My Extension' });
```

## Props

One component per tag — `<Card>`, `<Button>`, `<TableCell>`, 133 of them,
exported by name from the package root.

- **A property is its Rust name in camelCase.** `Label` is `label`, `RowSpan` is
  `rowSpan`. An unknown prop is an error, not a silent no-op.
- **The property decides the wire type, not the JavaScript value.** `gap={2}` is
  two spacing steps, `columns={2}` is two columns, `fraction={0.5}` is a number.
- **Closed vocabularies are written in kebab or camel case**: `tone="accent"`,
  `variant="outline"`, `scale="title"`, `align="center"`, `color="text-dim"`.
- **Lengths**: a number is steps on the 4px scale; `"fill"` and `"content"` are
  the named sizes; `{chars: 12}` is a text-relative width. `pad` also takes
  `{top, end, bottom, start}`, and `width`/`height` take `{minimum, maximum}`.
- **`null` or `undefined` means the host should forget the property** — it emits
  `ClearProp`.
- **Text children are the label.** `<Text>hello</Text>` and
  `<Text label="hello" />` are the same thing; bare text has no widget.
- **A handler is `on` plus the trigger**: `onInvoke`, `onChange`, `onSubmit`,
  `onSelect`, `onActivate`, `onToggle`, `onExpand`, `onScroll`, `onClose`,
  `onContext`. The event identity is derived from the node and the trigger, so
  re-rendering with a fresh closure rebinds locally and sends no patch. The
  callback receives `{trigger, node, id, value}`.

`vocabulary` exports both lists, and `tags` exports every component name.

## Tests

`npm test` — plain `node --test`, no framework.
