// @ts-nocheck -- legacy story typing is migrated incrementally.
// The library, as data.
//
// Every list in the playground — the sidebar's families, the components in
// them, the rows of the property editor and the members of each closed
// vocabulary — is read from here. Nothing about the component library is
// written down twice: `npm run catalogue` regenerates this document from the
// Rust side, and everything downstream follows.

import catalogue from './catalogue.json' with { type: 'json' };

/** The document shape this playground understands. */
export const SHAPE_VERSION = 1;

if (catalogue.version !== SHAPE_VERSION) {
  throw new Error(
    `the catalogue is shape ${catalogue.version} and this playground reads ${SHAPE_VERSION}; run npm run catalogue`,
  );
}

export const families = catalogue.families;
export const tags = catalogue.tags;
export const props = catalogue.props;
export const enums = catalogue.enums;
export const lengths = catalogue.lengths;
export const notes = catalogue.notes;

/** Every component, grouped by family, in catalogue order. */
export function grouped() {
  const byFamily = new Map(families.map((family) => [family.name, { ...family, tags: [] }]));
  for (const tag of tags) {
    const group = byFamily.get(tag.family);
    if (group === undefined) {
      throw new Error(`<${tag.name}> claims family ${tag.family}, which the catalogue does not declare`);
    }
    group.tags.push(tag);
  }
  return [...byFamily.values()].filter((family) => family.tags.length > 0);
}

const BY_NAME = new Map(tags.map((tag) => [tag.name, tag]));

/** One component's catalogue entry. */
export function component(name) {
  const tag = BY_NAME.get(name);
  if (tag === undefined) throw new Error(`<${name}> is not in the catalogue`);
  return tag;
}

/** The property as the React prop that carries it: `RowSpan` is `rowSpan`. */
export function camel(name) {
  return name[0].toLowerCase() + name.slice(1);
}

/** A closed vocabulary member as it is written in JSX: `TextDim` is `text-dim`. */
export function style(vocabulary, wire) {
  const member = (enums[vocabulary] ?? []).find((entry) => entry.wire === wire);
  return member === undefined ? wire : member.style;
}

/** The properties one component declares, in catalogue order. */
export function editable(name) {
  const declared = new Set(component(name).props);
  return props.filter((prop) => declared.has(prop.name));
}

/** The properties an inline control can actually edit, and the ones it cannot. */
export const CONTROLLABLE = new Set(['text', 'switch', 'enum', 'number', 'length', 'edges']);

/** The closed vocabulary an `enum` property draws its members from. */
export function vocabularyOf(prop) {
  return prop.values.find((name) => name in enums) ?? null;
}

/** The largest spacing step the style sheet has a class for. */
export function maximumStep() {
  const step = lengths.find((entry) => entry.shape === 'Step');
  return step?.maximum ?? 12;
}

export default catalogue;
