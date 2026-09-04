// @ts-nocheck -- legacy story typing is migrated incrementally.
// Which control edits which property, and what the control produces.
//
// The choice is the catalogue's `editor` hint, not a list kept here — a
// property added to the library arrives with its own hint and gets a control
// without this file changing.

import { CONTROLLABLE, camel, editable, enums, maximumStep, style, vocabularyOf } from './catalogue.js';

/** Where the catalogue's hint and the JavaScript binding disagree. */
export const BINDING = {};

/** The control a property is edited with. */
export function editorOf(prop) {
  return BINDING[prop.name] ?? prop.editor;
}

/** A property, described as the editor needs it: one row of the right pane. */
export function control(prop) {
  const editor = editorOf(prop);
  const row = {
    prop: prop.name,
    name: camel(prop.name),
    group: prop.group,
    editor,
    note: prop.note,
    editable: CONTROLLABLE.has(editor),
  };
  if (editor === 'enum') {
    const vocabulary = vocabularyOf(prop);
    row.vocabulary = vocabulary;
    row.members = (enums[vocabulary] ?? []).map((member) => ({ value: member.style, label: member.style }));
  }
  if (editor === 'length' || editor === 'edges') {
    row.modes = LENGTH_MODES;
    row.maximum = maximumStep();
  }
  return row;
}

/** Every property as a row, in catalogue order, editable ones first. */
export function rows(name) {
  const all = editable(name).map(control);
  return [...all.filter((row) => row.editable), ...all.filter((row) => !row.editable)];
}

/** How a length is written: steps, characters, or one of the named sizes. */
export const LENGTH_MODES = [
  { value: 'step', label: 'steps' },
  { value: 'chars', label: 'chars' },
  { value: 'fill', label: 'fill' },
  { value: 'content', label: 'content' },
];

/** The mode a length value is currently in, so the control can show itself. */
export function modeOf(value) {
  if (value === 'fill' || value === 'content') return value;
  if (value && typeof value === 'object' && typeof value.chars === 'number') return 'chars';
  return 'step';
}

/** The amount a length value carries, for the modes that carry one. */
export function amountOf(value) {
  if (typeof value === 'number') return value;
  if (value && typeof value === 'object' && typeof value.chars === 'number') return value.chars;
  if (value && typeof value === 'object' && typeof value.step === 'number') return value.step;
  return 0;
}

/** A length value from the mode and the amount the two controls hold. */
export function lengthValue(mode, amount) {
  if (mode === 'fill' || mode === 'content') return mode;
  if (mode === 'chars') return { chars: Math.max(0, Math.round(amount)) };
  return Math.max(0, Math.round(amount));
}

/** The member a `Select` should show for the value a property holds. */
export function memberOf(prop, value) {
  const vocabulary = vocabularyOf(prop);
  return vocabulary === null ? String(value) : style(vocabulary, String(value));
}
