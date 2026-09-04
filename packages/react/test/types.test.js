import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import test from 'node:test';
import { fileURLToPath } from 'node:url';

const here = path.dirname(fileURLToPath(import.meta.url));
const catalogue = JSON.parse(fs.readFileSync(path.resolve(here, '../catalogue.json'), 'utf8'));
const declarations = fs.readFileSync(path.resolve(here, '../src/index.d.ts'), 'utf8');
const clientDeclarations = fs.readFileSync(path.resolve(here, '../../client/src/index.d.ts'), 'utf8');
const generatedClientDeclarations = fs.readFileSync(path.resolve(here, '../../client/src/generated-protocol.d.ts'), 'utf8');

/** The body of one component's interface, as generated. */
function shape(name) {
  const start = declarations.indexOf(`export interface ${name}Props extends NodeProps {`);
  assert.notEqual(start, -1, `<${name}> has no props interface`);
  return declarations.slice(start, declarations.indexOf('\n}', start));
}

test('every component in the catalogue has a props interface', () => {
  for (const tag of catalogue.tags) {
    assert.match(shape(tag.name), /\{/);
    assert.ok(
      declarations.includes(`export const ${tag.name}: ComponentType<${tag.name}Props>;`),
      `<${tag.name}> is declared as no component at all`,
    );
  }
});

test('a component offers what it declares and nothing else', () => {
  const button = shape('Button');
  assert.match(button, /tone\?: /, 'a button is toned');
  assert.match(button, /label\?: string;/);
  assert.match(button, /destructive\?: boolean;/, 'a dangerous action can require confirmation');
  assert.doesNotMatch(button, /schema\?/, 'a button holds no rows');
  assert.doesNotMatch(shape('Text'), /checked\?/, 'a label holds no state');
  assert.match(shape('DataTable'), /schema\?: ColumnSpec\[\];/);
});

test('a handler is offered only where the component reports that interaction', () => {
  for (const tag of catalogue.tags) {
    const written = shape(tag.name);
    for (const trigger of ['Invoke', 'Change', 'Toggle', 'Expand']) {
      const declared = tag.triggers.includes(trigger);
      assert.equal(
        written.includes(`on${trigger}?:`),
        declared,
        `<${tag.name}> ${declared ? 'declares' : 'does not declare'} ${trigger}`,
      );
    }
  }
});

test('the declarations were generated from this catalogue', () => {
  const enums = catalogue.enums.Tone.map((member) => JSON.stringify(member.style));
  for (const spelling of enums) {
    assert.ok(declarations.includes(spelling), `a tone is missing the spelling ${spelling}`);
  }
});

test('a render handle exposes the addressed multi-surface lifecycle', () => {
  const handle = declarations.match(/export interface RenderHandle \{(?<body>[\s\S]*?)\n\}/)?.groups?.body;
  assert.ok(handle, 'the generated declarations expose RenderHandle');
  assert.match(handle, /readonly ready: Promise<string>;/);
  assert.match(handle, /readonly slot: string \| null;/);
  assert.match(handle, /source\(mutation: InterfaceSourceMutation\): Promise<void>;/);
  assert.match(handle, /close\(\): void;/);
  assert.match(
    declarations,
    /split\?: \{ slot: string; division: 'beside' \| 'below' \}/,
    'the generated render options describe tab-to-split composition',
  );
});

test('host events type the pane chooser identity as well as subscribed snapshots', () => {
  assert.match(clientDeclarations, /export interface PaneSelection \{ pane_provider: string; slot: string \}/);
  assert.match(clientDeclarations, /export type InterfaceEvent = WireUiEvent;/);
  assert.match(clientDeclarations, /import type \{[^}]*\bWireUiEvent\b[^}]*\} from '\.\/generated-protocol\.js';/);
  assert.match(generatedClientDeclarations, /\{ interaction: "key" \} & \{ "trigger": string; "node": number; "id": string; "slot"\?: string \| null; "key": string; "keycode": number; "modifiers": number; "pressed": boolean \}/);
  assert.match(generatedClientDeclarations, /export type UiPointerPhase = "enter" \| "motion" \| "leave" \| "press" \| "release";/);
  assert.match(generatedClientDeclarations, /"x"\?: number \| null; "y"\?: number \| null; "button": number; "modifiers": number/);
  assert.match(clientDeclarations, /export type HostEvent = SnapshotEvent \| PaneSelection \| InterfaceEvent \| LegacyInterfaceEvent;/);
  assert.match(clientDeclarations, /onEvent\?: \(event: HostEvent, channel: number\) => void;/);
  assert.doesNotMatch(
    clientDeclarations,
    /onEvent\?: \(event: SnapshotEvent/,
    'strict consumers are incorrectly told that provider selections cannot arrive',
  );
});

test('windowed selection types immutable source generation and row identity', () => {
  assert.match(declarations, /interface SelectedCollectionRow \{ index: number; id: string; \}/);
  assert.match(declarations, /interface CollectionSelection \{ source: number; version: number; rows: SelectedCollectionRow\[\]; \}/);
  assert.match(shape('DataTable'), /onSelect\?: \(report: SelectionReport\) => void;/);
  assert.match(declarations, /collection\?: CollectionSelection \| null/);
});

test('event hooks expose typed lifecycle-safe subscriptions', () => {
  assert.match(
    declarations,
    /useHostEvents\(session: Session, listener: \(event: HostEvent, channel: number\) => void\): void;/,
  );
  assert.match(
    declarations,
    /usePaneSelection\(session: Session, provider\?: string \| null\): PaneSelection \| null;/,
  );
});
