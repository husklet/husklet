// The host config: React's tree, expressed as patches.
//
// An instance here is nothing but a node identity and the props last sent, so
// the whole renderer is a translation layer with no retained widgets on this
// side. The host owns the widgets; this owns the identities.

import Reconciler from 'react-reconciler';
import { DefaultEventPriority } from 'react-reconciler/constants.js';
import catalogue from '../catalogue.json' with { type: 'json' };
import { ROOT, children, partition, same } from './protocol.js';

const TAGS = new Map(catalogue.tags.map((entry) => [entry.name, entry]));

/**
 * Collects patches and hands them over one frame at a time.
 *
 * A frame is atomic on the host, so everything React commits together must
 * leave here together — the surface must never show half of a render.
 */
export class Surface {
  #next = 1;
  #sequence = 0;
  #queue = [];
  #handlers = new Map();
  #send;

  constructor(send, { sequence = 0, next = 1, patches = [] } = {}) {
    this.#send = send;
    this.#sequence = sequence;
    this.#next = next;
    this.#queue.push(...patches);
  }

  /** The sequence of the last frame sent. The host refuses a gap. */
  get sequence() {
    return this.#sequence;
  }

  allocate() {
    // Identifiers are never reused, so a late patch naming a removed node is
    // always detectable rather than ambiguous.
    return this.#next++;
  }

  push(patch) {
    this.#queue.push(patch);
  }

  /** Binds a callback to the identity the host will echo back. */
  bind(id, trigger, callback) {
    this.#handlers.set(`${id}:${trigger}`, callback);
  }

  unbind(id, trigger) {
    this.#handlers.delete(`${id}:${trigger}`);
  }

  /** Runs whatever the host reports against an identity this surface issued. */
  dispatch(event) {
    const callback = this.#handlers.get(event.id);
    if (!callback) return false;
    callback(event);
    return true;
  }

  /** Sends everything queued as one frame, and nothing at all when idle. */
  flush() {
    if (this.#queue.length === 0) return null;
    const frame = { sequence: ++this.#sequence, patches: this.#queue };
    this.#queue = [];
    this.#send(frame);
    return frame;
  }
}

function describe(tag) {
  const entry = TAGS.get(tag);
  if (entry === undefined) {
    throw new Error(`<${tag}> is not a component; import the ones that exist from @husklet/react`);
  }
  return entry;
}

/** Where a node actually attaches: a detached surface always sits at the root. */
function anchor(parent, child) {
  return child.detached ? ROOT : parent.id;
}

function setProps(surface, instance, values) {
  for (const [prop, value] of values) {
    surface.push({ SetProp: { id: instance.id, prop, value } });
  }
}

function setHandlers(surface, instance, handlers) {
  for (const [trigger, callback] of handlers) {
    const id = `${instance.id}:${trigger}`;
    surface.push({ SetHandler: { id: instance.id, handler: { trigger, id } } });
    surface.bind(instance.id, trigger, callback);
  }
}

/**
 * The difference between two renders, as patches.
 *
 * A handler's identity is derived from the node and the trigger, so re-rendering
 * with a fresh closure rebinds locally and sends nothing: the common case of a
 * component re-rendering unchanged costs an empty frame, which is no frame.
 */
function difference(surface, instance, before, after) {
  const patches = [];
  for (const [prop, value] of after.values) {
    if (before.values.has(prop) && same(before.values.get(prop), value)) continue;
    patches.push({ SetProp: { id: instance.id, prop, value } });
  }
  for (const prop of before.values.keys()) {
    if (!after.values.has(prop)) patches.push({ ClearProp: { id: instance.id, prop } });
  }
  for (const [trigger, callback] of after.handlers) {
    surface.bind(instance.id, trigger, callback);
    if (before.handlers.has(trigger)) continue;
    patches.push({ SetHandler: { id: instance.id, handler: { trigger, id: `${instance.id}:${trigger}` } } });
  }
  for (const trigger of before.handlers.keys()) {
    if (after.handlers.has(trigger)) continue;
    surface.unbind(instance.id, trigger);
    patches.push({ ClearHandler: { id: instance.id, trigger } });
  }
  return patches;
}

const config = {
  supportsMutation: true,
  supportsPersistence: false,
  supportsHydration: false,
  isPrimaryRenderer: true,
  noTimeout: -1,
  supportsMicrotasks: true,
  scheduleMicrotask: queueMicrotask,
  scheduleTimeout: setTimeout,
  cancelTimeout: clearTimeout,

  getRootHostContext: () => null,
  getChildHostContext: (parent) => parent,
  getPublicInstance: (instance) => instance,
  getCurrentEventPriority: () => DefaultEventPriority,
  getInstanceFromNode: () => null,
  getInstanceFromScope: () => null,
  beforeActiveInstanceBlur() {},
  afterActiveInstanceBlur() {},
  prepareScopeUpdate() {},
  preparePortalMount() {},
  detachDeletedInstance() {},

  createInstance(type, props, surface) {
    const entry = describe(type);
    const instance = { id: surface.allocate(), tag: type, detached: entry.detached, parent: null, surface };
    surface.push({ Create: { id: instance.id, tag: type } });
    const split = partition(type, props);
    instance.props = split;
    setProps(surface, instance, split.values);
    setHandlers(surface, instance, split.handlers);
    return instance;
  },

  createTextInstance(text, surface, context, fiber) {
    void surface;
    void context;
    void fiber;
    throw new Error(
      `bare text ${JSON.stringify(text)} has no widget; put it inside a component, where it becomes the label`,
    );
  },

  // Text children are the node's label, not a child node, so React must not
  // reconcile them as one.
  shouldSetTextContent: (type, props) => children(props) !== null,
  resetTextContent() {},
  commitTextUpdate() {},

  appendInitialChild(parent, child) {
    parent.surface.push({ Insert: { parent: anchor(parent, child), child: child.id, before: null } });
    child.parent = anchor(parent, child);
  },

  finalizeInitialChildren: () => false,

  appendChild(parent, child) {
    attach(parent.surface, anchor(parent, child), child, null);
  },

  appendChildToContainer(surface, child) {
    attach(surface, ROOT, child, null);
  },

  insertBefore(parent, child, before) {
    attach(parent.surface, anchor(parent, child), child, before.id);
  },

  insertInContainerBefore(surface, child, before) {
    attach(surface, ROOT, child, before.id);
  },

  removeChild(parent, child) {
    parent.surface.push({ Remove: { id: child.id } });
  },

  removeChildFromContainer(surface, child) {
    surface.push({ Remove: { id: child.id } });
  },

  clearContainer() {},

  prepareUpdate(instance, type, before, after) {
    const split = partition(type, after);
    const patches = difference(instance.surface, instance, instance.props, split);
    instance.pending = split;
    return patches.length === 0 ? null : patches;
  },

  commitUpdate(instance, patches) {
    instance.props = instance.pending ?? instance.props;
    for (const patch of patches) instance.surface.push(patch);
  },

  prepareForCommit: () => null,

  resetAfterCommit(surface) {
    surface.flush();
  },
};

/**
 * Attaches or reorders, which the host distinguishes and React does not.
 *
 * React calls the same hook to place a new child and to move an existing one;
 * only the renderer knows which, because only it knows where the node is now.
 */
function attach(surface, parent, child, before) {
  const patch = { parent, child: child.id, before };
  surface.push(child.parent === parent ? { Move: patch } : { Insert: patch });
  child.parent = parent;
}

export const reconciler = Reconciler(config);
