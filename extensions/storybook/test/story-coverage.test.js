import assert from 'node:assert/strict';
import test from 'node:test';
import { createElement as h } from 'react';

import { Playground } from '../dist/app.js';
import { AcquisitionProgressStory } from '../dist/acquisition.js';
import { ValidatedSettingsFormStory } from '../dist/form.js';
import { KeyboardAccessibilityStory } from '../dist/keyboard-accessibility.js';
import { LargeDataTableStory, LargeRecordSource } from '../dist/large-table.js';
import { NAVIGATION_STORY, NavigationDialogsStory } from '../dist/navigation-dialogs.js';
import { StreamingLogStory } from '../dist/streaming-log.js';
import { EventStreamStory, TimelineSource } from '../dist/event-stream.js';
import { KeyValueInspectorStory, KeyValueSource } from '../dist/key-value-inspector.js';
import { MarkdownReviewStory } from '../dist/markdown-review.js';
import { storyCoverage } from '../dist/story-coverage.js';
import { DIFF_STORY, DiffReviewStory } from '../dist/diff-review.js';
import { JsonResponseStory } from '../dist/json-response.js';
import { StackTraceStory } from '../dist/stack-trace.js';
import { BinaryInspectionStory } from '../dist/binary-inspection.js';
import { ResourceMetricsStory } from '../dist/resource-metrics.js';
import { FileBrowserStory } from '../dist/file-browser.js';
import { ProfileInspectionStory, boundedFrames, FRAME_LIMIT } from '../dist/profile-inspection.js';
import { MemoryInspectionStory, boundedRegions, REGION_LIMIT } from '../dist/memory-inspection.js';
import { DisassemblyInspectionStory, boundedInstructions, INSTRUCTION_LIMIT } from '../dist/disassembly-inspection.js';
import { TimelineInspectionStory, boundedEvents, TIMELINE_LIMIT } from '../dist/timeline-inspection.js';
import { TestReportStory, boundedCases, CASE_LIMIT, FAILURE_LIMIT } from '../dist/test-report.js';
import { CoverageInspectionStory, boundedCoverage, COVERAGE_LIMIT, SOURCE_LIMIT } from '../dist/coverage-inspection.js';
import { NetworkWaterfallStory, boundedRequests, REQUEST_LIMIT, PHASE_LIMIT } from '../dist/network-waterfall.js';
import { DependencyGraphStory, boundedGraph, NODE_LIMIT } from '../dist/dependency-graph.js';
import { JsonTreeStory } from '../dist/json-tree.js';
import { ConfirmationStory } from '../dist/confirmation.js';
import { ContainerOperationsStory, boundedContainers, CONTAINER_LIMIT, PROCESS_LIMIT, LOG_LIMIT } from '../dist/container-operations.js';
import { WorkspaceLayoutStory, boundedPanes, retainEvents, EVENT_LIMIT, PANE_LIMIT, TITLE_LIMIT } from '../dist/workspace-layout.js';
import { ExtensionLifecycleStory, boundedExtensions, EXTENSION_LIMIT, GRANT_LIMIT, FIELD_LIMIT } from '../dist/extension-lifecycle.js';
import { WorkspaceFileEditStory, boundedFiles, FILE_LIMIT, PATH_LIMIT, CONTENT_LIMIT } from '../dist/workspace-file-edit.js';
import { ImagePullStory, boundedPull, LAYER_LIMIT, REFERENCE_LIMIT, STATUS_LIMIT } from '../dist/image-pull.js';
import { ResourceStateStory } from '../dist/resource-state.js';
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

test('every composed story has a readable root and a bounded initial wire frame', () => {
  const stories = [
    ['safe destructive confirmation', h(ConfirmationStory)],
    ['container operations', h(ContainerOperationsStory)],
    ['workspace layout', h(WorkspaceLayoutStory)],
    ['extension lifecycle', h(ExtensionLifecycleStory)],
    ['workspace file edit', h(WorkspaceFileEditStory)],
    ['image pull', h(ImagePullStory)],
    ['container inventory states', h(ResourceStateStory)],
    ['bounded JSON tree', h(JsonTreeStory)],
    ['acquisition', h(AcquisitionProgressStory)],
    ['validated form', h(ValidatedSettingsFormStory)],
    ['keyboard accessibility', h(KeyboardAccessibilityStory)],
    ['large records', h(LargeDataTableStory, { source: new LargeRecordSource() })],
    ['navigation', h(NavigationDialogsStory)],
    ['streaming log', h(StreamingLogStory)],
    ['event timeline', h(EventStreamStory, { source: new TimelineSource() })],
    ['key/value inspector', h(KeyValueInspectorStory, { source: new KeyValueSource() })],
    ['diff review', h(DiffReviewStory)],
    ['JSON response', h(JsonResponseStory)],
    ['stack trace', h(StackTraceStory)],
    ['markdown review', h(MarkdownReviewStory)],
    ['binary inspection', h(BinaryInspectionStory)],
    ['resource metrics', h(ResourceMetricsStory)],
    ['file browser', h(FileBrowserStory)],
    ['profile inspection', h(ProfileInspectionStory)],
    ['memory inspection', h(MemoryInspectionStory)],
    ['disassembly inspection', h(DisassemblyInspectionStory)],
    ['timeline view', h(TimelineInspectionStory)],
    ['test report', h(TestReportStory)],
    ['coverage inspection', h(CoverageInspectionStory)],
    ['network waterfall', h(NetworkWaterfallStory)],
    ['dependency graph', h(DependencyGraphStory)],
  ];
  for (const [name, story] of stories) {
    const frame = host().render(story);
    const labels = frame.patches.filter((patch) => patch.SetProp?.prop === 'Label')
      .map((patch) => patch.SetProp.value.Text);
    assert(frame.patches.some((patch) => patch.Create?.tag === 'Heading'), `${name} has no semantic heading root`);
    assert(labels.some((label) => typeof label === 'string' && label.trim().length > 0), `${name} has no readable label`);
    assert(frame.patches.length <= 256, `${name} emitted ${frame.patches.length} initial patches`);
  }
});

test('container inventory story traverses failure, retry, empty, and ready states', () => {
  const stage = host();
  const initial = stage.render(h(ResourceStateStory));
  const error = node(initial.patches, 'Button', 'error');
  assert(error);
  let before = stage.frames.length;
  assert(stage.surface.dispatch({ trigger: 'Invoke', node: error, id: `${error}:Invoke` }));
  let changed = stage.since(before);
  assert(node(changed, 'Button', 'Retry inventory'));

  const retry = node(changed, 'Button', 'Retry inventory');
  before = stage.frames.length;
  assert(stage.surface.dispatch({ trigger: 'Invoke', node: retry, id: `${retry}:Invoke` }));
  changed = stage.since(before);
  assert(changed.some((patch) => patch.Create?.tag === 'Progress'));

  const all = stage.frames.flatMap((frame) => frame.patches);
  const ready = node(all, 'Button', 'ready');
  before = stage.frames.length;
  assert(stage.surface.dispatch({ trigger: 'Invoke', node: ready, id: `${ready}:Invoke` }));
  changed = stage.since(before);
  assert(node(changed, 'ListItemText', 'api · running'));
  assert(changed.length <= 64);
});

test('container operations bounds and sanitizes every host-sized projection', () => {
  const containers = Array.from({ length: CONTAINER_LIMIT + 3 }, (_, index) => ({
    id: `immutable-${index}`, name: `container-${index}\nunsafe`, image: 'x'.repeat(140), state: 'invented',
    logs: 'l'.repeat(LOG_LIMIT + 20),
    processes: Array.from({ length: PROCESS_LIMIT + 4 }, (_, pid) => ({ pid, user: 'user', command: 'c'.repeat(200) })),
  }));
  const bounded = boundedContainers(containers);
  assert.equal(bounded.length, CONTAINER_LIMIT);
  assert.equal(bounded[0].processes.length, PROCESS_LIMIT);
  assert.equal(bounded[0].logs.length, LOG_LIMIT);
  assert.equal(bounded[0].state, 'unknown');
  assert(!bounded[0].name.includes('\n'));
  assert.equal(bounded[0].processes[0].command.length, 160);
});

test('workspace layout bounds slots and interactively splits by stable identity', () => {
  const source = Array.from({ length: PANE_LIMIT + 4 }, (_, index) => ({
    slot: `pane-${index}`, title: `title-${index}\n${'x'.repeat(TITLE_LIMIT)}`, occupant: 'invented',
  }));
  const bounded = boundedPanes(source);
  assert.equal(bounded.length, PANE_LIMIT);
  assert.equal(bounded[0].occupant, 'empty');
  assert.equal(bounded[0].title.length, TITLE_LIMIT);
  assert(!bounded[0].title.includes('\n'));
  assert.equal(retainEvents(Array.from({ length: 9 }, (_, index) => String(index)), 'latest').length, EVENT_LIMIT);

  const stage = host();
  const first = stage.render(h(WorkspaceLayoutStory));
  const split = node(first.patches, 'Button', 'Split below');
  const chooser = node(first.patches, 'Button', 'Open pane chooser');
  const focus = node(first.patches, 'Button', 'Focus selected pane');
  assert(split);
  assert(chooser && focus, 'chooser and keyboard focus controls are visible');
  assert(first.patches.some((patch) => patch.SetProp?.value?.Text === 'nested horizontal split'));
  assert(first.patches.some((patch) => patch.SetProp?.value?.Text?.includes('top/containers')));
  let before = stage.frames.length;
  assert(stage.surface.dispatch({ trigger: 'Invoke', node: chooser, id: `${chooser}:Invoke` }));
  let changed = stage.since(before);
  assert(changed.some((patch) => patch.SetProp?.value?.Text?.includes('top/containers')));
  before = stage.frames.length;
  assert(stage.surface.dispatch({ trigger: 'Invoke', node: split, id: `${split}:Invoke` }));
  changed = stage.since(before);
  assert(changed.some((patch) => patch.SetProp?.prop === 'Orientation'));
  assert(changed.some((patch) => patch.SetProp?.value?.Text === 'Split pane-terminal-1 below into pane-new-4.'));
  assert(changed.some((patch) => patch.SetProp?.value?.Text === 'pane-new-4 · terminal'));
});

test('extension lifecycle bounds authority and controls the selected immutable generation', () => {
  const source = Array.from({ length: EXTENSION_LIMIT + 3 }, (_, index) => ({
    name: `extension-${index}`, version: 'v'.repeat(FIELD_LIMIT + 10), digest: `sha256:generation-${index}`,
    status: 'invented', grants: Array.from({ length: GRANT_LIMIT + 4 }, (_, grant) => `grant-${grant}`),
  }));
  const bounded = boundedExtensions(source);
  assert.equal(bounded.length, EXTENSION_LIMIT);
  assert.equal(bounded[0].grants.length, GRANT_LIMIT);
  assert.equal(bounded[0].version.length, FIELD_LIMIT);
  assert.equal(bounded[0].status, 'failed');

  const stage = host();
  const first = stage.render(h(ExtensionLifecycleStory));
  const stop = node(first.patches, 'Button', 'Stop extension');
  assert(stop);
  const before = stage.frames.length;
  assert(stage.surface.dispatch({ trigger: 'Invoke', node: stop, id: `${stop}:Invoke` }));
  const changed = stage.since(before);
  assert(changed.some((patch) => patch.SetProp?.value?.Text === 'Start extension'));
  assert(changed.some((patch) => patch.SetProp?.value?.Text === 'Stopped top at sha256:manager-generation-14.'));
});

test('workspace file review independently bounds paths, content, and interactive writes', () => {
  const source = Array.from({ length: FILE_LIMIT + 3 }, (_, index) => ({
    path: index === 1 ? '../escape' : `src/${'p'.repeat(PATH_LIMIT)}-${index}.js`,
    content: 'x'.repeat(CONTENT_LIMIT + 20),
  }));
  const bounded = boundedFiles(source);
  assert.equal(bounded.length, FILE_LIMIT - 1);
  assert.equal(bounded[0].path.length, PATH_LIMIT);
  assert.equal(bounded[0].content.length, CONTENT_LIMIT);
  assert(!bounded.some(({ path }) => path.includes('..')));

  const stage = host();
  const first = stage.render(h(WorkspaceFileEditStory));
  const rename = node(first.patches, 'Button', 'Rename for review');
  assert(rename);
  const before = stage.frames.length;
  assert(stage.surface.dispatch({ trigger: 'Invoke', node: rename, id: `${rename}:Invoke` }));
  const changed = stage.since(before);
  assert(changed.some((patch) => patch.SetProp?.value?.Text === 'Renamed src/server.js to src/server.review.js.'));
  assert(changed.some((patch) => patch.SetProp?.value?.Text?.startsWith('src/server.review.js · ')));
});

test('image pull independently bounds progress, target, digest, error, and cancellation', () => {
  const pull = boundedPull({
    job: '42', reference: 'r'.repeat(REFERENCE_LIMIT + 9), platform: 'linux/s390x', state: 'invented',
    digest: 'sha256:short', error: `failure\n${'e'.repeat(STATUS_LIMIT + 9)}`,
    layers: Array.from({ length: LAYER_LIMIT + 4 }, (_, index) => ({ id: `layer-${index}`, current: 20, total: 10 })),
  });
  assert.equal(pull.layers.length, LAYER_LIMIT);
  assert.equal(pull.layers[0].current, 10);
  assert.equal(pull.reference.length, REFERENCE_LIMIT);
  assert.equal(pull.platform, 'linux/amd64');
  assert.equal(pull.state, 'failed');
  assert.equal(pull.digest, '');
  assert.equal(pull.error.length, STATUS_LIMIT);
  assert(!pull.error.includes('\n'));

  const stage = host();
  const first = stage.render(h(ImagePullStory));
  const cancel = node(first.patches, 'Button', 'Cancel pull');
  assert(cancel);
  const before = stage.frames.length;
  assert(stage.surface.dispatch({ trigger: 'Invoke', node: cancel, id: `${cancel}:Invoke` }));
  const changed = stage.since(before);
  assert(changed.some((patch) => patch.SetProp?.value?.Text === 'Retry pull'));
  assert(changed.some((patch) => patch.SetProp?.value?.Text === 'Cancelled image-pull job 42; the existing local image is unchanged.'));
  assert(!changed.some((patch) => patch.SetProp?.value?.Text?.startsWith('sha256:')));
});
test('dependency graph bounds and interactively filters issues',()=>{const graph=boundedGraph({nodes:Array.from({length:NODE_LIMIT+2},(_,i)=>({id:`n${i}`,label:`n${i}`,version:'1',state:i?'resolved':'conflict',detail:'x'})),edges:[],cycles:[],totals:{nodes:99,edges:0,cycles:0}});assert.equal(graph.nodes.length,NODE_LIMIT);const stage=host();const first=stage.render(h(DependencyGraphStory));const filter=node(first.patches,'Button','Show issues only');const before=stage.frames.length;assert(stage.surface.dispatch({trigger:'Invoke',node:filter,id:`${filter}:Invoke`}));const changed=stage.since(before);assert(changed.some(p=>p.Remove));assert(changed.some(p=>p.SetProp?.value?.Text==='Show all'))});

test('network waterfall validates, caps, sanitizes, and exposes typed hierarchy', () => {
  const phases = Array.from({length: PHASE_LIMIT}, (_, i) => ({kind:'wait',offsetUs:i*2,durationUs:2}));
  const requests = Array.from({length: REQUEST_LIMIT + 3}, (_, i) => ({method:'GET',url:`https://example.test/${i}\nunsafe`,startUs:0,durationUs:20,status:200,bytes:1,detail:'ok\t',phases}));
  requests.push({...requests[0], method:'TRACE'}); requests.push({...requests[0], phases:[{kind:'wait',offsetUs:4,durationUs:4},{kind:'dns',offsetUs:2,durationUs:3}]});
  const bounded = boundedRequests(requests, 90); assert.equal(bounded.requests.length, REQUEST_LIMIT); assert.equal(bounded.total, 90);
  const frame = host().render(h(NetworkWaterfallStory));
  assert.equal(frame.patches.filter((p) => p.Create?.tag === 'NetworkRequest').length, 3);
  assert(frame.patches.some((p) => p.Create?.tag === 'NetworkPhase'));
  assert(!frame.patches.some((p) => p.SetProp?.value?.Text?.includes('\nunsafe')));
  const stage = host(); const initial = stage.render(h(NetworkWaterfallStory));
  const filter = node(initial.patches, 'Button', 'Show failures only'); assert(filter);
  const before = stage.frames.length; assert(stage.surface.dispatch({ trigger:'Invoke', node:filter, id:`${filter}:Invoke` }));
  const changed = stage.since(before); assert(changed.some((p) => p.SetProp?.prop === 'Label' && p.SetProp.value.Text === 'Show all requests'));
  assert(changed.some((p) => p.Remove), 'filtering must remove successful requests');
});

test('coverage inspection bounds rows and source independently with visible truncation', () => {
  const lines = Array.from({ length: COVERAGE_LIMIT + 4 }, (_, index) => ({ line: index + 1, hits: index % 2, source: 'x'.repeat(SOURCE_LIMIT + 9) })); const value = boundedCoverage(lines, 900);
  assert.equal(value.split('\n').length, COVERAGE_LIMIT + 1); assert.equal(value.split('\n')[0].split('\t')[2].length, SOURCE_LIMIT); assert(value.endsWith(`… showing ${COVERAGE_LIMIT} of 900 lines …`));
  assert.equal(boundedCoverage([null, { line: -1, hits: 0 }]), '…\t\t… showing 0 of 2 lines …');
});

test('test report bounds cases and failure detail independently', () => {
  const cases = Array.from({ length: CASE_LIMIT + 3 }, (_, index) => ({ suite: 'api', name: `case-${index}`, status: 'failed', durationMs: index, failure: 'x'.repeat(FAILURE_LIMIT + 20) }));
  cases.splice(1, 0, null, { suite: '', name: 'invalid', status: 'passed', durationMs: 1, failure: '' }); const value = boundedCases(cases);
  assert.equal(value.split('\n').length, CASE_LIMIT); assert(!value.includes('invalid')); assert.equal(value.split('\n')[0].split('\t')[4].length, FAILURE_LIMIT);
});

test('timeline view rejects blank events and enforces its hard ceiling', () => {
  const events = Array.from({ length: TIMELINE_LIMIT + 5 }, (_, index) => ({ timestampMs: index, category: 'runtime', label: `event-${index}`, detail: 'observed' }));
  events.splice(1, 0, null, { timestampMs: 1, category: 'runtime', label: '', detail: 'blank' });
  const value = boundedEvents(events); assert.equal(value.split('\n').length, TIMELINE_LIMIT); assert(!value.includes('blank')); assert(value.startsWith('0\truntime\tevent-0\tobserved'));
});

test('disassembly inspection rejects invalid instructions and enforces its hard ceiling', () => {
  const instructions = Array.from({ length: INSTRUCTION_LIMIT + 7 }, (_, index) => ({ address: index, bytes: [0xc3], mnemonic: 'ret', operands: '' }));
  instructions.splice(1, 0, null, { address: 2, bytes: [], mnemonic: 'bad', operands: '' }, { address: 3, bytes: [256], mnemonic: 'wide', operands: '' });
  const value = boundedInstructions(instructions);
  assert.equal(value.split('\n').length, INSTRUCTION_LIMIT);
  assert(!value.includes('\t\tbad\t'));
  assert(!value.includes('\twide\t'));
  assert(value.startsWith('0000000000000000\tc3\tret\t'));
});

test('memory inspection rejects invalid regions and enforces its hard ceiling', () => {
  const regions = Array.from({ length: REGION_LIMIT + 9 }, (_, index) => ({ start: index * 4096, end: (index + 1) * 4096, permissions: 'r-xp', mapping: `segment-${index}` }));
  regions.splice(1, 0, null, { start: 4, end: 4, permissions: 'rw-p', mapping: 'empty' });
  const value = boundedRegions(regions);
  assert.equal(value.split('\n').length, REGION_LIMIT);
  assert(!value.includes('empty'));
  assert(value.startsWith('0000000000000000-0000000000001000\tr-xp\t4096\tsegment-0'));
});

test('profile inspection rejects invalid frames and enforces its hard ceiling', () => {
  const frames = Array.from({ length: FRAME_LIMIT + 17 }, (_, index) => ({ label: `frame-${index}`, samples: index + 1 }));
  frames.splice(2, 0, null, { label: 'idle', samples: 0 }, { label: '', samples: 4 });
  const value = boundedFrames(frames);
  assert.equal(value.split('\n').length, FRAME_LIMIT);
  assert(!value.includes('idle'));
  assert(value.startsWith('1\tframe-0'));
});

test('the diff review is bounded, selectable, and switches presentation', () => {
  const stage = host();
  const first = stage.render(h(Playground));
  const story = node(first.patches, 'ListItemButton', DIFF_STORY);
  assert.ok(story);
  stage.surface.dispatch({ trigger: 'Invoke', node: story, id: `${story}:Invoke` });
  const opened = stage.frames.at(-1).patches;
  assert.equal(opened.filter((patch) => patch.Create?.tag === 'DiffLine').length, 4);
  const toggle = node(opened, 'Button', 'Show side by side');
  assert.ok(toggle);
  const before = stage.frames.length;
  stage.surface.dispatch({ trigger: 'Invoke', node: toggle, id: `${toggle}:Invoke` });
  assert.ok(stage.since(before).some((patch) => patch.SetProp?.prop === 'Orientation'));
});

test('navigation and transient UI is selectable and demonstrates expand, invoke, and close', () => {
  const stage = host();
  const first = stage.render(h(Playground));
  const story = node(first.patches, 'ListItemButton', NAVIGATION_STORY);
  assert.ok(story);
  stage.surface.dispatch({ trigger: 'Invoke', node: story, id: `${story}:Invoke` });
  const opened = stage.frames.at(-1).patches;
  const accordion = opened.find((patch) => patch.Create?.tag === 'Accordion')?.Create.id;
  const palette = opened.find((patch) => patch.Create?.tag === 'CommandPalette')?.Create.id;
  const button = node(opened, 'Button', 'Open actions');
  assert.ok(accordion && palette && button);
  stage.surface.dispatch({ trigger: 'Change', node: palette, id: `${palette}:Change`, value: 'logs' });
  stage.surface.dispatch({ trigger: 'Submit', node: palette, id: `${palette}:Submit` });
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
  assert.ok(labels.includes('Command submitted: logs.'));
});
