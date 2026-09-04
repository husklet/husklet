// The render path, without a host: the reconciler collects patches into frames
// and the test reads them, exactly as @husklet/react's own tests do.

import assert from 'node:assert/strict';
import test from 'node:test';
import { createElement as h } from 'react';

import {
  FLOW_STORIES,
  Playground,
  Preview,
  Sidebar,
  SEARCH_RESULT_LIMIT,
  interactionDetail,
  interactionProps,
  searchResults,
} from '../dist/app.js';
import { grouped, tags } from '../dist/catalogue.js';
import { defaults } from '../dist/defaults.js';
import { components } from '@husklet/react';
import { ACQUISITION_STORY, acquisitionStates } from '../dist/acquisition.js';
import { FORM_STORY, ValidatedSettingsFormStory } from '../dist/form.js';
import { EVENT_LIMIT, KEYBOARD_STORY, KeyboardAccessibilityStory } from '../dist/keyboard-accessibility.js';
import { QUERY_PLAN_MODES, QueryPlanStory, filterQueryPlan, queryPlan } from '../dist/query-plan.js';

import { host } from './host.js';

/** The nodes a frame created, as `{id, tag}`. */
function created(patches) {
  return patches.filter((patch) => 'Create' in patch).map((patch) => patch.Create);
}

/** The identity of the first node created with the given tag and label. */
function node(patches, tag, label) {
  let candidate = null;
  for (const patch of patches) {
    if ('Create' in patch && patch.Create.tag === tag) candidate = patch.Create.id;
    if (
      candidate !== null &&
      'SetProp' in patch &&
      patch.SetProp.id === candidate &&
      patch.SetProp.prop === 'Label' &&
      patch.SetProp.value.Text === label
    ) {
      return candidate;
    }
  }
  return null;
}

test('every component renders with its defaults as sane patches', () => {
  for (const tag of tags) {
    const stage = host();
    const opened = defaults(tag.name);
    const frame = stage.render(
      h(
        components[tag.name],
        opened.props,
        ...opened.children.map((child, index) => h(components[child.tag], { key: index, ...child.props })),
      ),
    );
    assert.ok(frame, `<${tag.name}> rendered nothing at all`);
    const roots = created(frame.patches).filter((entry) => entry.tag === tag.name);
    assert.equal(roots.length, 1, `<${tag.name}> is not a single instance of itself`);
    assert.ok(
      frame.patches.some((patch) => 'SetProp' in patch && patch.SetProp.id === roots[0].id) ||
        opened.children.length > 0,
      `<${tag.name}> arrived with no properties and no children`,
    );
  }
});

test('the playground renders flows and only one bounded component family', () => {
  const stage = host();
  const frame = stage.render(h(Playground));
  const built = created(frame.patches).map((entry) => entry.tag);
  assert.equal(frame.sequence, 1, 'a commit is one atomic frame');
  assert.equal(built.filter((tag) => tag === 'Row').length >= 1, true);
  assert.equal(
    built.filter((tag) => tag === 'ListItemButton').length,
    FLOW_STORIES.length + grouped().find((family) => family.name === 'buttons').tags.length,
    'flows and the active family are listed',
  );
  assert.ok(built.filter((tag) => tag === 'ListItemButton').length < tags.length / 2);
  assert.ok(built.includes('Scroll'), 'the sidebar and the inspector scroll');
  assert.ok(built.includes('Select') && built.includes('Switch') && built.includes('NumberEntry'));
});

test('the sidebar uses one native scroller without nesting a List scroller', () => {
  const frame = host().render(h(Sidebar, {
    families: grouped(), selected: 'Button', activeFamily: 'buttons',
    onFamily: () => {}, onSelect: () => {},
  }));
  const built = created(frame.patches).map((entry) => entry.tag);
  assert.equal(built.filter((tag) => tag === 'Scroll').length, 1);
  assert.equal(built.filter((tag) => tag === 'List').length, 0, 'nested native scrollers collapse the navigation');
});

test('global navigation finds an unknown-family component without materializing the catalogue', () => {
  const families = grouped();
  const broad = searchResults(families, 'a');
  assert.equal(broad.length, SEARCH_RESULT_LIMIT, 'global results have no hard rendering bound');
  assert.ok(searchResults(families, 'datatable').some((result) => result.name === 'DataTable'));

  const stage = host();
  const first = stage.render(h(Playground));
  const search = first.patches.find((patch) => patch.SetProp?.prop === 'Placeholder'
    && patch.SetProp.value.Text === 'Search flows and components')?.SetProp.id;
  assert.ok(search, 'global navigation is not discoverable before the long flow list');
  const beforeSearch = stage.frames.length;
  assert.ok(stage.surface.dispatch({ trigger: 'Change', node: search, id: `${search}:Change`, value: 'DataTable' }));
  const matches = stage.since(beforeSearch);
  const dataTable = node(matches, 'ListItemButton', 'DataTable');
  assert.ok(dataTable, 'search still requires knowing the component family');
  assert.ok(!node(matches, 'ListItemButton', FLOW_STORIES[0]), 'search retained the unrelated flow catalogue');

  const beforeSelect = stage.frames.length;
  assert.ok(stage.surface.dispatch({ trigger: 'Invoke', node: dataTable, id: `${dataTable}:Invoke` }));
  assert.ok(node(stage.since(beforeSelect), 'Heading', 'Data Table'));
});

function labels(patches, tag) {
  const ids = new Set(created(patches).filter((entry) => entry.tag === tag).map((entry) => entry.id));
  return patches
    .filter((patch) => patch.SetProp?.prop === 'Label' && ids.has(patch.SetProp.id))
    .map((patch) => patch.SetProp.value.Text);
}

function setsText(patches, prop, text) {
  return patches.some((patch) => patch.SetProp?.prop === prop && patch.SetProp.value.Text === text);
}

test('query plan filters retain matching operators and every ancestor, but no unrelated sibling', () => {
  const hot = filterQueryPlan(queryPlan, 'hotspot');
  assert.equal(hot.id, 'root');
  assert.deepEqual(hot.children.map((child) => child.id), ['join']);
  assert.deepEqual(hot.children[0].children.map((child) => child.id), ['orders-hash']);
  assert.deepEqual(hot.children[0].children[0].children.map((child) => child.id), ['orders']);
  const mismatch = filterQueryPlan(queryPlan, 'mismatch');
  assert.equal(mismatch.id, 'root');
  assert.deepEqual(mismatch.children.map((child) => child.id), ['preferences']);
  assert.equal(filterQueryPlan(queryPlan, 'full').children.length, 2);
});

test('query plan callbacks switch among full, hotspot, and mismatch projections', () => {
  const stage = host();
  const first = stage.render(h(QueryPlanStory));
  assert.equal(labels(first.patches, 'QueryPlanNode').length, 6);
  const all = stage.frames.flatMap((frame) => frame.patches);
  const hotspot = node(all, 'Button', QUERY_PLAN_MODES.hotspot);
  const mismatch = node(all, 'Button', QUERY_PLAN_MODES.mismatch);
  const full = node(all, 'Button', QUERY_PLAN_MODES.full);
  assert.ok(hotspot && mismatch && full, 'the three projections are not independently selectable');

  let before = stage.frames.length;
  assert.ok(stage.surface.dispatch({ trigger: 'Invoke', node: hotspot, id: `${hotspot}:Invoke`, value: null }));
  let patches = stage.since(before);
  assert.ok(setsText(patches, 'Label', 'Showing hotspots with their ancestor paths.'));
  assert.ok(setsText(patches, 'Label', '4 plan operators'));
  assert.ok(patches.some((patch) => 'Remove' in patch), 'hotspot filtering retained unrelated siblings');

  before = stage.frames.length;
  assert.ok(stage.surface.dispatch({ trigger: 'Invoke', node: mismatch, id: `${mismatch}:Invoke`, value: null }));
  patches = stage.since(before);
  assert.ok(setsText(patches, 'Label', 'Showing estimate mismatches with their ancestor paths.'));
  assert.ok(setsText(patches, 'Label', '2 plan operators'));
  assert.deepEqual(labels(patches, 'QueryPlanNode'), ['subquery_scan · Preference summary']);

  before = stage.frames.length;
  assert.ok(stage.surface.dispatch({ trigger: 'Invoke', node: full, id: `${full}:Invoke`, value: null }));
  patches = stage.since(before);
  assert.ok(setsText(patches, 'Label', 'Showing the complete captured plan.'));
  assert.ok(setsText(patches, 'Label', '6 plan operators'));
  assert.equal(labels(patches, 'QueryPlanNode').length, 4, 'the four filtered operators were not restored');
});

test('keyboard accessibility story validates, confirms separately, and bounds focus history', () => {
  const stage = host();
  const first = stage.render(h(KeyboardAccessibilityStory));
  const entry = created(first.patches).find((created) => created.tag === 'Entry')?.id;
  const review = node(first.patches, 'Button', 'Review removal');
  const disabled = node(first.patches, 'Button', 'Unavailable');
  assert.ok(entry && review && disabled);
  assert.ok(first.patches.some((patch) => 'SetProp' in patch && patch.SetProp.id === disabled
    && patch.SetProp.prop === 'Enabled' && patch.SetProp.value.Flag === false));

  let before = stage.frames.length;
  stage.surface.dispatch({ trigger: 'Invoke', node: review, id: `${review}:Invoke`, value: null });
  assert.ok(node(stage.since(before), 'InlineMessage', 'Resolve the validation error before confirmation.'));

  stage.surface.dispatch({ trigger: 'Change', node: entry, id: `${entry}:Change`, value: 'storybook' });
  before = stage.frames.length;
  stage.surface.dispatch({ trigger: 'Invoke', node: review, id: `${review}:Invoke`, value: null });
  const confirmation = stage.since(before);
  assert.ok(node(confirmation, 'Button', 'Cancel'));
  const confirm = node(confirmation, 'Button', 'Confirm removal');
  assert.ok(confirm);
  assert.ok(confirmation.some((patch) => 'SetProp' in patch && patch.SetProp.id === confirm
    && patch.SetProp.prop === 'Destructive' && patch.SetProp.value.Flag === true));

  for (let index = 0; index < EVENT_LIMIT + 3; index += 1) {
    stage.surface.dispatch({ trigger: 'Focus', node: review, id: `${review}:Focus`, focused: true });
  }
  const labels = stage.frames.flatMap((frame) => frame.patches)
    .filter((patch) => 'SetProp' in patch && patch.SetProp.prop === 'Label')
    .map((patch) => patch.SetProp.value.Text);
  assert.ok(labels.includes(`Event history (${EVENT_LIMIT}/${EVENT_LIMIT})`));
  assert.ok(!labels.some((label) => label?.includes('ERROR disabled control focused')));
});

test('keyboard accessibility story is selectable from the shipped sidebar', () => {
  const stage = host();
  const first = stage.render(h(Playground));
  const item = node(first.patches, 'ListItemButton', KEYBOARD_STORY);
  assert.ok(item);
  const before = stage.frames.length;
  stage.surface.dispatch({ trigger: 'Invoke', node: item, id: `${item}:Invoke`, value: null });
  assert.ok(node(stage.since(before), 'Heading', 'Keyboard-safe extension removal'));
});

test('the form story validates submit, recovers on change, and confirms success', () => {
  const stage = host();
  const first = stage.render(h(ValidatedSettingsFormStory));
  const entry = created(first.patches).find((created) => created.tag === 'Entry')?.id;
  const save = node(first.patches, 'Button', 'Save defaults');
  assert.ok(entry && save, 'the form has no editable field or save action');

  let before = stage.frames.length;
  assert.ok(stage.surface.dispatch({ trigger: 'Submit', node: entry, id: `${entry}:Submit`, value: null }));
  let patches = stage.since(before);
  assert.ok(node(patches, 'ValidationSummary', 'Fix workspace name.'));
  const review = node(patches, 'Button', 'Review workspace name');
  assert.ok(review, 'validation summary has no corrective action');
  const beforeReview = stage.frames.length;
  stage.surface.dispatch({ trigger: 'Invoke', node: review, id: `${review}:Invoke` });
  assert.ok(stage.since(beforeReview).some((patch) => patch.SetProp?.value?.Text === 'Ready to correct.'));
  assert.ok(
    patches.some((patch) => 'SetProp' in patch && patch.SetProp.prop === 'Tone' && patch.SetProp.value.Tone === 'Danger'),
    'invalid submission does not mark the field or feedback as dangerous',
  );

  before = stage.frames.length;
  assert.ok(stage.surface.dispatch({ trigger: 'Change', node: entry, id: `${entry}:Change`, value: 'api' }));
  patches = stage.since(before);
  assert.ok(patches.some((patch) => 'Remove' in patch), 'correcting the field leaves stale validation feedback');

  before = stage.frames.length;
  assert.ok(stage.surface.dispatch({ trigger: 'Invoke', node: save, id: `${save}:Invoke`, value: null }));
  patches = stage.since(before);
  assert.ok(node(patches, 'Banner', 'Defaults saved for api.'), 'valid submission has no success confirmation');
});

test('the tag input retains a submitted value and removes only the activated tag', () => {
  const stage = host();
  const first = stage.render(h(ValidatedSettingsFormStory));
  const input = created(first.patches).find((entry) => entry.tag === 'TagInput')?.id;
  const backend = node(first.patches, 'ToggleButton', 'backend');
  assert.ok(input && backend);

  stage.surface.dispatch({ trigger: 'Change', node: input, id: `${input}:Change`, value: 'urgent' });
  const beforeAdd = stage.frames.length;
  stage.surface.dispatch({ trigger: 'Submit', node: input, id: `${input}:Submit` });
  assert.ok(node(stage.since(beforeAdd), 'ToggleButton', 'urgent'));

  const beforeRemove = stage.frames.length;
  stage.surface.dispatch({ trigger: 'Toggle', node: backend, id: `${backend}:Toggle`, value: false });
  const removed = stage.since(beforeRemove);
  assert.ok(removed.some((patch) => 'Remove' in patch), JSON.stringify(removed));
});

test('the validated form is selectable as a canonical end-user flow', () => {
  const stage = host();
  const first = stage.render(h(Playground));
  const item = node(first.patches, 'ListItemButton', FORM_STORY);
  assert.ok(item, 'the sidebar omits the form flow');
  const before = stage.frames.length;
  assert.ok(stage.surface.dispatch({ trigger: 'Invoke', node: item, id: `${item}:Invoke`, value: null }));
  const patches = stage.since(before);
  assert.ok(node(patches, 'Heading', 'Workspace defaults'), 'selecting the form flow does not render it');
});

test('the acquisition flow selects every semantic progress state without materializing them together', () => {
  const stage = host();
  const first = stage.render(h(Playground));
  const item = node(first.patches, 'ListItemButton', ACQUISITION_STORY);
  assert.ok(item, 'the sidebar has no acquisition flow');
  const before = stage.frames.length;
  assert.ok(stage.surface.dispatch({ trigger: 'Invoke', node: item, id: `${item}:Invoke`, value: null }));
  const initial = stage.since(before);
  const select = created(initial).find((entry) => entry.tag === 'Select')?.id;
  assert.ok(select, 'the lifecycle state selector is absent');
  const patches = [...initial];
  for (const [index, state] of acquisitionStates.entries()) {
    const start = stage.frames.length;
    if (index !== 0) {
      assert.ok(stage.surface.dispatch({ trigger: 'Change', node: select, id: `${select}:Change`, value: state.key }));
      patches.push(...stage.since(start));
    }
    const visible = index === 0 ? initial : stage.since(start);
    assert.ok(node(visible, 'CardHeader', state.title), `${state.key} is absent`);
    assert.ok(node(visible, 'InlineMessage', state.status), `${state.key} has no semantic status`);
  }
  for (const action of ['Cancel download', 'Retry', 'Install', 'Cancel']) {
    assert.ok(node(patches, 'Button', action), `${action} is not demonstrated`);
  }
  assert.equal(
    created(patches).filter((entry) => entry.tag === 'Progress').length,
    1,
    'only measured transfer claims a fraction',
  );
  assert.equal(
    created(patches).filter((entry) => entry.tag === 'Spinner').length,
    3,
    'checking, unknown transfer and manifest read remain indeterminate',
  );
});

test('the preview is a real instance of the selected component', () => {
  const stage = host();
  const frame = stage.render(h(Playground));
  assert.ok(
    created(frame.patches).some((entry) => entry.tag === 'Button'),
    'the component the playground opens on is rendered, not described',
  );
});

test('the preview demonstrates declared interactions with a live bounded console', () => {
  const stage = host();
  const opened = defaults('Button');
  const first = stage.render(h(Preview, {
    name: 'Button',
    opened,
    triggers: ['Invoke', 'Key'],
  }));
  const preview = node(first.patches, 'Button', 'Button');
  assert.ok(preview, 'the interactive preview button is absent');
  assert.ok(node(first.patches, 'InlineMessage', 'Interact with the preview to inspect onInvoke, onKey.'));

  const before = stage.frames.length;
  assert.ok(stage.surface.dispatch({ trigger: 'Invoke', node: preview, id: `${preview}:Invoke`, value: null }));
  const patches = stage.since(before);
  assert.ok(
    patches.some(
      (patch) => 'SetProp' in patch
        && patch.SetProp.prop === 'Label'
        && patch.SetProp.value.Text === '#1 Invoke received · value=null',
    ),
    'a real preview event never reaches the visible console',
  );
});

test('interaction handlers follow the catalogue and payload descriptions stay bounded', () => {
  const seen = [];
  const handlers = interactionProps(['Change', 'Focus'], (trigger, event) => seen.push([trigger, event]));
  assert.deepEqual(Object.keys(handlers), ['onChange', 'onFocus']);
  handlers.onFocus({ focused: true });
  assert.deepEqual(seen, [['Focus', { focused: true }]]);
  assert.equal(interactionDetail({ key: 'a', pressed: true, private: 'not shown' }), 'key="a" pressed=true');
  const long = interactionDetail({ value: 'x'.repeat(500) });
  assert.ok(long.length <= 240);
  assert.ok(long.endsWith('…"'), 'a long value is not visibly marked as truncated');
  assert.equal(
    interactionDetail({ rows: Array.from({ length: 100_000 }, (_, index) => ({ index, private: 'not shown' })) }),
    'rows=[{"index":0,"private":"not shown"},{"index":1,"private":"not shown"},{"index":2,"private":"not shown"},"… 99997 more"]',
  );
});

test('the interaction console preserves a bounded sequence and can be cleared', () => {
  const stage = host();
  const opened = defaults('Button');
  const first = stage.render(h(Preview, { name: 'Button', opened, triggers: ['Invoke'] }));
  const preview = node(first.patches, 'Button', 'Button');
  for (let index = 0; index < 7; index += 1) {
    assert.ok(stage.surface.dispatch({ trigger: 'Invoke', node: preview, id: `${preview}:Invoke`, value: index }));
  }
  const labels = stage.frames.flatMap((frame) => frame.patches)
    .filter((patch) => 'SetProp' in patch && patch.SetProp.prop === 'Label')
    .map((patch) => patch.SetProp.value.Text);
  assert.ok(labels.includes('#7 Invoke received · value=6'), 'the newest interaction is absent');

  const latest = stage.frames.at(-1).patches;
  const removed = latest.filter((patch) => 'Remove' in patch).length;
  assert.ok(removed > 0, 'the oldest interaction was not evicted from the bounded timeline');
  const clear = node(stage.frames.flatMap((frame) => frame.patches), 'Button', 'Clear');
  assert.ok(clear, 'the populated console has no clear action');
  const before = stage.frames.length;
  assert.ok(stage.surface.dispatch({ trigger: 'Invoke', node: clear, id: `${clear}:Invoke`, value: null }));
  assert.ok(node(stage.since(before), 'InlineMessage', 'Interact with the preview to inspect onInvoke.'));
});

test('selecting a component in the sidebar renders that component', () => {
  const stage = host();
  const first = stage.render(h(Playground));
  const family = created(first.patches).find((entry) => entry.tag === 'Select').id;
  stage.surface.dispatch({ trigger: 'Change', node: family, id: `${family}:Change`, value: 'display' });
  const item = node(stage.frames.flatMap((frame) => frame.patches), 'ListItemButton', 'Chip');
  assert.ok(item, 'the sidebar has no row for <Chip>');
  const before = stage.frames.length;
  assert.ok(stage.surface.dispatch({ trigger: 'Invoke', node: item, id: `${item}:Invoke`, value: null }));
  const patches = stage.since(before);
  assert.ok(
    created(patches).some((entry) => entry.tag === 'Chip'),
    'selecting <Chip> did not render a <Chip>',
  );
});

test('the inspector follows the selected component contract and shows its interactions', () => {
  const stage = host();
  const first = stage.render(h(Playground));
  const family = created(first.patches).find((entry) => entry.tag === 'Select').id;
  stage.surface.dispatch({ trigger: 'Change', node: family, id: `${family}:Change`, value: 'forms' });
  const item = node(stage.frames.flatMap((frame) => frame.patches), 'ListItemButton', 'Switch');
  assert.ok(item, 'the sidebar has no row for <Switch>');
  const before = stage.frames.length;
  assert.ok(stage.surface.dispatch({ trigger: 'Invoke', node: item, id: `${item}:Invoke`, value: null }));
  const patches = stage.since(before);
  assert.ok(node(patches, 'Text', 'checked'), '<Switch> does not expose its checked property');
  assert.ok(node(patches, 'Text', 'onToggle'), '<Switch> does not expose its Toggle interaction');
  assert.equal(node(patches, 'Text', 'label'), null, '<Switch> exposes Button-only label editing');
});

test('family navigation reaches every catalogue component without simultaneous materialization', () => {
  const stage = host();
  const first = stage.render(h(Playground));
  const selector = created(first.patches).find((entry) => entry.tag === 'Select').id;
  const seen = new Set();
  for (const family of grouped()) {
    stage.surface.dispatch({ trigger: 'Change', node: selector, id: `${selector}:Change`, value: family.name });
    const labels = stage.frames.at(-1).patches.filter((patch) => patch.SetProp?.prop === 'Label')
      .map((patch) => patch.SetProp.value.Text);
    for (const tag of family.tags) assert.ok(labels.includes(tag.name), `${family.name} omits ${tag.name}`);
    family.tags.forEach((tag) => seen.add(tag.name));
  }
  assert.deepEqual([...seen].sort(), tags.map((tag) => tag.name).sort());
});

test('global search input is bounded and keeps the selected story visible', () => {
  const stage = host();
  const first = stage.render(h(Playground));
  const selector = created(first.patches).find((entry) => entry.tag === 'Select').id;
  const search = created(first.patches).find((entry) => entry.tag === 'Entry').id;
  assert.ok(stage.surface.dispatch({ trigger: 'Change', node: selector, id: `${selector}:Change`, value: 'content' }));
  assert.ok(stage.surface.dispatch({ trigger: 'Change', node: search, id: `${search}:Change`, value: `Log${'x'.repeat(100)}` }));
  const values = stage.frames.flatMap((frame) => frame.patches).filter((patch) =>
    patch.SetProp?.id === search && patch.SetProp.prop === 'Value');
  assert.equal(values.at(-1).SetProp.value.Text.length, 80);
  assert.ok(node(stage.frames.flatMap((frame) => frame.patches), 'Heading', 'Button'), 'active preview disappears while browsing');
  assert.ok(node(stage.frames.flatMap((frame) => frame.patches), 'Text', 'No flows or components match this search.'));
});

test('the inspector exposes only the genuine extended interactions', () => {
  const triggers = tags.find((tag) => tag.name === 'IconButton').triggers;
  for (const interaction of ['onKey', 'onFocus', 'onPointer', 'onContext']) {
    assert.ok(triggers.includes(interaction.slice(2)), `<IconButton> does not expose ${interaction}`);
  }
  assert.equal(triggers.includes('Scroll'), false, '<IconButton> invents scrolling');
});

test('editing a property re-renders the preview with the new value', () => {
  const stage = host();
  const first = stage.render(h(Playground));
  // The preview's button carries the default label; the inspector's Entry for
  // `label` is the one bound to Change beside the row named "label".
  const entry = labelEntry(first.patches);
  assert.ok(entry, 'the inspector has no text field for the label');
  const before = stage.frames.length;
  assert.ok(stage.surface.dispatch({ trigger: 'Change', node: entry, id: `${entry}:Change`, value: 'Pressed' }));
  const patches = stage.since(before);
  assert.ok(
    patches.some(
      (patch) => 'SetProp' in patch && patch.SetProp.prop === 'Label' && patch.SetProp.value.Text === 'Pressed',
    ),
    'the new label never reached the host',
  );
});

/** The identity of the inspector's `label` field: the Entry after that row's name. */
function labelEntry(patches) {
  let seenRow = false;
  let entry = null;
  for (const patch of patches) {
    if (
      'SetProp' in patch &&
      patch.SetProp.prop === 'Label' &&
      patch.SetProp.value.Text === 'label' &&
      !seenRow
    ) {
      seenRow = true;
      continue;
    }
    if (seenRow && 'Create' in patch && patch.Create.tag === 'Entry') {
      entry = patch.Create.id;
      break;
    }
  }
  return entry;
}
