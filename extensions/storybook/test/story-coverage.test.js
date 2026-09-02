import assert from 'node:assert/strict';
import test from 'node:test';
import { createElement as h } from 'react';

import { Playground } from '../src/app.js';
import { NAVIGATION_STORY } from '../src/navigation-dialogs.js';
import { storyCoverage } from '../src/story-coverage.js';
import { host } from './host.js';

function difference(expected, actual) {
  return [...expected].filter((item) => !actual.has(item));
}

function node(patches, tag, label) {
  let candidate = null;
  for (const patch of patches) {
    if (patch.Create?.tag === tag) candidate = patch.Create.id;
    if (candidate !== null && patch.SetProp?.id === candidate && patch.SetProp.prop === 'Label'
      && patch.SetProp.value.Text === label) return candidate;
  }
  return null;
}

test('every catalogue contract has a meaningful selectable state and family coverage', () => {
  const coverage = storyCoverage();
  assert.deepEqual(difference(coverage.expectedFamilies, coverage.families), []);
  assert.deepEqual(difference(coverage.expectedPropertyGroups, coverage.propertyGroups), []);
  assert.deepEqual(difference(coverage.expectedInteractions, coverage.interactions), []);
  for (const story of coverage.componentStories) {
    assert(story.state.children.length > 0 || Object.keys(story.state.props).length > 0,
      `${story.component} has no meaningful visible story state`);
    assert(story.propertyGroups.length > 0, `${story.component} demonstrates no property family`);
  }
});

test('navigation and transient UI is selectable and demonstrates expand, invoke, and close', () => {
  const stage = host();
  const first = stage.render(h(Playground));
  const story = node(first.patches, 'ListItemButton', NAVIGATION_STORY);
  assert.ok(story);
  stage.surface.dispatch({ trigger: 'Invoke', node: story, id: `${story}:Invoke` });
  const opened = stage.frames.at(-1).patches;
  const accordion = opened.find((patch) => patch.Create?.tag === 'Accordion')?.Create.id;
  const button = node(opened, 'Button', 'Open actions');
  assert.ok(accordion && button);
  stage.surface.dispatch({ trigger: 'Expand', node: accordion, id: `${accordion}:Expand`, expanded: false });
  const beforeMenu = stage.frames.length;
  stage.surface.dispatch({ trigger: 'Invoke', node: button, id: `${button}:Invoke` });
  const menuFrame = stage.since(beforeMenu);
  const popover = menuFrame.find((patch) => patch.Create?.tag === 'Popover')?.Create.id;
  assert.ok(popover);
  stage.surface.dispatch({ trigger: 'Close', node: popover, id: `${popover}:Close` });
  const labels = stage.frames.flatMap((frame) => frame.patches)
    .filter((patch) => patch.SetProp?.prop === 'Label').map((patch) => patch.SetProp.value.Text);
  assert.ok(labels.includes('Deployment details collapsed.'));
  assert.ok(labels.includes('Action menu dismissed.'));
});
