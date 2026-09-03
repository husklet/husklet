// What a component looks like the moment it is selected.
//
// A blank preview teaches nothing, so every component opens with enough
// properties set to be visible. The shape of the default set is decided by the
// catalogue — a component that takes children gets children, one that does not
// gets a label — and the taste, the particular words and numbers, is the only
// thing written down here.

import { component, tags } from './catalogue.js';

/** Children a container opens with, so an empty box is never shown as one. */
const SAMPLE = [
  { tag: 'Text', props: { label: 'One' } },
  { tag: 'Text', props: { label: 'Two' } },
  { tag: 'Text', props: { label: 'Three' } },
];

/** What a family wants beyond its label, before any per-component taste. */
const BY_FAMILY = {
  layout: { gap: 2, pad: 2 },
  surface: { pad: 2 },
  display: {},
  feedback: {},
  buttons: { variant: 'filled' },
  fields: { placeholder: 'Type here' },
  forms: {},
  lists: {},
  tables: {},
  trees: {},
  navigation: {},
  dialogs: { pad: 2 },
  content: {},
};

/** The components whose point is not a label. */
const BY_TAG = {
  Icon: { icon: 'star' },
  Avatar: { label: 'HK' },
  Image: { uri: 'https://example.invalid/picture.png' },
  ImageListItem: { uri: 'https://example.invalid/picture.png' },
  Video: { uri: 'https://example.invalid/clip.webm' },
  Link: { uri: 'https://example.invalid/', label: 'A link' },
  Progress: { fraction: 0.4 },
  Meter: { fraction: 0.4 },
  Spinner: { busy: true },
  Skeleton: { width: 24, height: 2 },
  Spacer: { height: 4 },
  Separator: { orientation: 'horizontal' },
  Grid: { columns: 2 },
  Splitter: { orientation: 'horizontal', position: 160 },
  Slider: { value: 5, minimum: 0, maximum: 10, step: 1 },
  NumberEntry: { value: 3, minimum: 0, maximum: 10, step: 1 },
  Rating: { value: 3, maximum: 5 },
  Stat: { label: 'Containers', value: '12' },
  Switch: { checked: true },
  Checkbox: { checked: true },
  Radio: { checked: true },
  ToggleButton: { checked: true },
  Expander: { expanded: true },
  Accordion: { expanded: true },
  Entry: { value: 'Editable text' },
  TextField: { value: 'Editable text' },
  TextArea: { value: 'Several\nlines' },
  Search: { placeholder: 'Search' },
  PasswordEntry: { value: 'hunter2', secret: true },
  Autocomplete: { placeholder: 'Start typing' },
  Select: { choices: [{ value: 'one', label: 'One' }, { value: 'two', label: 'Two' }] },
  RadioGroup: { choices: [{ value: 'one', label: 'One' }, { value: 'two', label: 'Two' }] },
  Code: { label: 'cargo test', monospace: true },
  CodeView: { label: 'fn main() {}', monospace: true },
  HexView: { value: '00000000  7f 45 4c 46                                      |.ELF|', monospace: true },
  LogView: { label: 'starting…', monospace: true },
  Chart: { label: 'Load' },
  Sparkline: { value: '18,22,19,31,28,35,42,39' },
  FlameGraph: { value: '120\tcompiler::parse\n74\tcompiler::check\n31\tcompiler::emit' },
  MemoryMap: { value: '0000000000400000-0000000000410000\tr-xp\t65536\t/bin/app' },
  DisassemblyView: { value: '0000000000401000\t55\tpush\trbp\n0000000000401001\t48 89 e5\tmov\trbp, rsp' },
  TimelineView: { value: '1700000000123\tdeploy\trelease started\tv2\n1700000001456\thealth\tready\t3 replicas' },
  TestReportView: { value: 'api\tcreates user\tpassed\t14\t\napi\trejects duplicate\tfailed\t8\texpected 409' },
  CoverageView: { value: '1\t3\tfn main() {\n2\t0\t    unreachable!();' },
  Badge: { label: '3' },
  Chip: { label: 'tag', variant: 'outline' },
  Toast: { label: 'Saved', tone: 'positive' },
  Banner: { label: 'Something happened', tone: 'warning' },
  InlineMessage: { label: 'Not quite right', tone: 'danger' },
  EmptyState: { label: 'Nothing here yet', detail: 'Add something to begin' },
  Table: { columns: 2 },
  DataTable: { columns: 2 },
  TreeTable: { columns: 2 },
  EventStream: { columns: 3 },
  FileBrowser: { columns: 3 },
  TableCell: { label: 'Cell' },
  TablePagination: { label: '1–10 of 40' },
  Pagination: { label: '1 of 4' },
  Heading: { label: 'A heading', scale: 'title' },
  Text: { label: 'Some text' },
};

/**
 * The props and children a component opens with.
 *
 * Returns children as plain descriptors rather than elements, so the default
 * set can be inspected and tested without React and without a host.
 */
export function defaults(name) {
  const tag = component(name);
  const props = { ...(BY_FAMILY[tag.family] ?? {}), ...(BY_TAG[name] ?? {}) };
  if (props.label === undefined && !tag.acceptsChildren) props.label = spaced(name);
  if (props.label === undefined && BY_TAG[name] === undefined && !LABELLESS.has(tag.family)) {
    props.label = spaced(name);
  }
  const children = tag.acceptsChildren ? SAMPLE.map((child) => ({ ...child })) : [];
  return { props, children };
}

/** Families whose containers read better without a caption of their own. */
const LABELLESS = new Set(['layout']);

/** `CardHeader` reads as `Card header` in a preview. */
export function spaced(name) {
  return name.replace(/([a-z])([A-Z])/g, '$1 $2');
}

/** Every component's default set, for tests and for a first selection. */
export function all() {
  return new Map(tags.map((tag) => [tag.name, defaults(tag.name)]));
}

/** The component the playground opens on. */
export const OPENING = 'Button';
