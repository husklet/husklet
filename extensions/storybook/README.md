# @husklet/storybook

A component playground, written as a Husklet extension in React. Three panes:

- **Left** — every component in the library, grouped by family.
- **Middle** — the selected component, rendered live. A real instance, not a
  picture of one.
- **Right** — its properties, one row each, with the control the property asks
  for. Editing a value re-renders the preview.

## Where it gets its knowledge

Everything — the families, the 133 components, each component's properties and
interactions, the 42-property vocabulary, the members of each closed
vocabulary, and which control edits which property — is read from
`src/catalogue.json`, which the library emits about itself:

```sh
npm run catalogue     # cargo run -p hl-gui --bin catalogue > src/catalogue.json
```

Nothing about the component library is written down twice, so the playground
cannot describe last month's library.

The inspector offers exactly the properties the selected component declares,
puts the properties an inline control can edit before the ones that must be
written in code, and lists the React handlers for every interaction that
component can report.

## Running it

```sh
npm install
npm test          # node --test, no framework

docker build \
  --build-arg HUSKLET_REACT_IMAGE=ghcr.io/husklet/husklet/extension-react-base:0.1.0 \
  --build-arg HUSKLET_REACT_VERSION=0.1.0 \
  --build-arg HUSKLET_EXTENSION_VERSION=0.1.0 \
  -t husklet/storybook .
```

The host starts the image, mounts a socket at `HUSKLET_EXTENSION_SOCKET`, and
reads the manifest off the image label; `src/main.js` connects and renders.
## Large DataTable story

The DataTable preview is backed by 100,000 logical records through Husklet's
bounded Source window protocol. Its controls change producer-side sorting,
filtering, and ready/loading/empty/error states. Scrolling or resizing requests
another window; no story creates one React node per record.
