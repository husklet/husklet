// The vocabulary translation: React props in, typed patches out.
//
// Every name in here is checked against the Rust side rather than invented:
// `Prop` and `Trigger` come from src/workspaces/hl-gui/src/node/prop.rs, the
// value shapes from that file and src/workspaces/hl-gui/src/style.rs.

/** The implicit container every top-level node is inserted into. */
export const ROOT = 0;

/**
 * How a property's value is spelled on the wire.
 *
 * The kind belongs to the property, not to the JavaScript value: `gap={2}`
 * means two spacing steps and `columns={2}` means two columns, and nothing
 * about the number `2` says which. A table keeps that decision in one place.
 */
const KIND = {
  // Content
  Label: 'text',
  Detail: 'text',
  Value: 'infer',
  Placeholder: 'text',
  Help: 'text',
  Icon: 'text',
  Tooltip: 'text',
  Uri: 'text',
  // State
  Enabled: 'flag',
  Visible: 'flag',
  Selected: 'flag',
  Checked: 'flag',
  Expanded: 'flag',
  Busy: 'flag',
  Secret: 'flag',
  Destructive: 'flag',
  Monospace: 'flag',
  Wrap: 'flag',
  Ellipsize: 'flag',
  // Appearance
  Variant: 'variant',
  Tone: 'tone',
  Scale: 'scale',
  Color: 'token',
  // Layout
  Gap: 'length',
  Pad: 'edges',
  // A growth factor, not a switch: the host reads it as a number and a
  // boolean would decode as nothing at all, silently refusing to expand.
  Grow: 'factor',
  Width: 'bounds',
  Height: 'bounds',
  Align: 'align',
  Justify: 'align',
  Columns: 'integer',
  Span: 'integer',
  RowSpan: 'integer',
  Orientation: 'orientation',
  Position: 'number',
  // Range
  Minimum: 'number',
  Maximum: 'number',
  Step: 'number',
  Fraction: 'number',
  // Collection
  Schema: 'schema',
  Source: 'source',
  RowHeight: 'number',
  Choices: 'choices',
};

/** Every property, spelled as the React prop that carries it. */
export const PROPS = new Map(Object.keys(KIND).map((prop) => [camel(prop), prop]));

/** Every trigger, spelled as the React prop that carries its callback. */
export const TRIGGERS = new Map(
  ['Invoke', 'Change', 'Submit', 'Select', 'Edit', 'Activate', 'Toggle', 'Expand', 'Scroll', 'Close', 'Context', 'Key', 'Focus', 'Pointer', 'Drag', 'Drop'].map(
    (trigger) => [`on${trigger}`, trigger],
  ),
);

/** Props React owns; they never reach the host. */
export const RESERVED = new Set(['children', 'key', 'ref']);

const VARIANTS = ['Plain', 'Filled', 'Outline', 'Ghost'];
const TONES = ['Neutral', 'Accent', 'Positive', 'Warning', 'Danger'];
const SCALES = ['Caption', 'Body', 'Title', 'Display'];
const ALIGNS = ['Start', 'Center', 'End', 'Stretch'];
const ORIENTATIONS = ['Horizontal', 'Vertical'];
const TOKENS = [
  'Ground',
  'Surface',
  'Raised',
  'Line',
  'Text',
  'TextDim',
  'TextFaint',
  'Accent',
  'Positive',
  'Warning',
  'Danger',
  'Info',
];

function camel(name) {
  return name[0].toLowerCase() + name.slice(1);
}

/** `accent`, `text-dim` and `TextDim` all name the same variant. */
function pascal(value) {
  return String(value)
    .split(/[-_\s]+/)
    .map((part) => part[0].toUpperCase() + part.slice(1))
    .join('');
}

function member(value, permitted, what) {
  const chosen = pascal(value);
  if (!permitted.includes(chosen)) {
    throw new Error(`${what} is one of ${permitted.map(camel).join(', ')}, not ${JSON.stringify(value)}`);
  }
  return chosen;
}

/**
 * A spacing or sizing quantity.
 *
 * A bare number is steps on the 4px scale, because that is what almost every
 * description means; the named sizes and the character width are spelled out.
 */
export function length(value) {
  if (typeof value === 'number') return { Step: Math.max(0, Math.round(value)) };
  if (value === 'fill') return 'Fill';
  if (value === 'content') return 'Content';
  if (value && typeof value === 'object') {
    if (typeof value.chars === 'number') return { Chars: Math.max(0, Math.round(value.chars)) };
    if (typeof value.step === 'number') return { Step: Math.max(0, Math.round(value.step)) };
  }
  throw new Error(`a length is a number of steps, "fill", "content", {chars} or {step}, not ${JSON.stringify(value)}`);
}

/** Translates one React prop value into a tagged `PropValue`. */
export function value(prop, given) {
  const kind = KIND[prop];
  switch (kind) {
    case 'text':
      return { Text: String(given) };
    case 'flag':
      return { Flag: Boolean(given) };
    case 'number':
      return { Number: Number(given) };
    // Written as a boolean by most people and as a weight by some; both mean
    // the same thing to the host, which compares the number against zero.
    case 'factor':
      return { Number: given === true ? 1 : given === false ? 0 : Number(given) };
    case 'integer':
      return { Integer: Math.round(Number(given)) };
    case 'variant':
      return { Variant: member(given, VARIANTS, 'a variant') };
    case 'tone':
      return { Tone: member(given, TONES, 'a tone') };
    case 'scale':
      return { Scale: member(given, SCALES, 'a scale') };
    case 'align':
      return { Align: member(given, ALIGNS, 'an alignment') };
    case 'orientation':
      return { Orientation: member(given, ORIENTATIONS, 'an orientation') };
    case 'token':
      return { Token: member(given, TOKENS, 'a color token') };
    case 'length':
      return { Length: length(given) };
    case 'edges':
      return edges(given);
    case 'bounds':
      return bounds(given);
    case 'source':
      return { Source: Math.round(Number(given)) };
    case 'choices':
      return { Choices: given.map((choice) => ({ value: String(choice.value), label: String(choice.label) })) };
    case 'schema':
      return { Schema: given.map(column) };
    case 'infer':
      return infer(given);
    default:
      throw new Error(`no such property ${prop}`);
  }
}

/** A value whose shape the property does not decide, such as a field's value. */
function infer(given) {
  if (typeof given === 'boolean') return { Flag: given };
  if (typeof given === 'number') return Number.isInteger(given) ? { Integer: given } : { Number: given };
  return { Text: String(given) };
}

function edges(given) {
  if (given === null) return { Nothing: null };
  if (typeof given !== 'object' || Array.isArray(given)) return { Length: length(given) };
  const sides = ['top', 'end', 'bottom', 'start'];
  if (!sides.some((side) => side in given)) return { Length: length(given) };
  const zero = { Step: 0 };
  return { Edges: Object.fromEntries(sides.map((side) => [side, side in given ? length(given[side]) : zero])) };
}

function bounds(given) {
  if (given && typeof given === 'object' && ('minimum' in given || 'maximum' in given)) {
    return {
      Bounds: {
        minimum: 'minimum' in given ? length(given.minimum) : null,
        maximum: 'maximum' in given ? length(given.maximum) : null,
      },
    };
  }
  return { Length: length(given) };
}

function column(given) {
  return {
    key: String(given.key),
    title: String(given.title ?? given.key),
    width: given.width === undefined ? 'Content' : length(given.width),
    align: given.align === undefined ? 'Start' : member(given.align, ALIGNS, 'an alignment'),
    sortable: Boolean(given.sortable),
    editable: Boolean(given.editable),
  };
}

/**
 * Splits a React element's props into what the host understands.
 *
 * An unknown prop is refused rather than dropped: a misspelled `lable` that
 * silently does nothing is the worst possible failure for someone writing an
 * interface they cannot inspect.
 */
export function partition(type, props) {
  const values = new Map();
  const handlers = new Map();
  for (const [name, given] of Object.entries(props)) {
    if (RESERVED.has(name)) continue;
    const trigger = TRIGGERS.get(name);
    if (trigger !== undefined) {
      if (typeof given !== 'function' && given !== null && given !== undefined) {
        throw new Error(`${name} on <${type}> takes a function`);
      }
      if (given) handlers.set(trigger, given);
      continue;
    }
    const prop = PROPS.get(name);
    if (prop === undefined) {
      throw new Error(`<${type}> has no prop ${name}; see the property table in README.md`);
    }
    // Absent and null both mean "the host should forget this property".
    if (given === undefined || given === null) continue;
    values.set(prop, value(prop, given));
  }
  // Text children are the label; a leaf tag has nowhere else to put them.
  const text = children(props);
  if (text !== null) values.set('Label', { Text: text });
  return { values, handlers };
}

/** The text a node's children amount to, or null when they are elements. */
export function children(props) {
  const given = props.children;
  if (typeof given === 'string') return given;
  if (typeof given === 'number') return String(given);
  return null;
}

/** Whether two tagged values would tell the host anything new. */
export function same(left, right) {
  return JSON.stringify(left) === JSON.stringify(right);
}
