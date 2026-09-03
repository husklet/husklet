// The conversation with the host: connect, greet, call, and answer.

import net from 'node:net';
import { CONTROL, FLAG_END, KIND, Reader, encode } from './wire.js';
import {
  encodeRequest, PROTOCOL_CAPABILITIES, PROTOCOL_REQUEST_CAPABILITIES, PROTOCOL_TOPICS, PROTOCOL_VERSION,
  validateFailure, validateReplyFor, validateSnapshot,
} from './generated-protocol.js';

/** The protocol this package speaks. The host refuses anything else. */
export const PROTOCOL = PROTOCOL_VERSION;

/** Where the host mounts the socket inside an extension's container. */
export const SOCKET = 'HUSKLET_EXTENSION_SOCKET';

/** The protocol's shared call channel. Replies are ordered on this channel. */
const CALLS = 2;
const ERROR = 2;
const COALESCED = 4;
const CLOSE_TIMEOUT = 1_000;
const SNAPSHOT_TOPICS = new Map(PROTOCOL_TOPICS.map(({ wire, snapshot }) => [snapshot, wire]));
const TOPIC_CAPABILITIES = new Map(PROTOCOL_TOPICS.map(({ wire, capability }) => [wire, capability]));
const CAPABILITIES = new Set(PROTOCOL_CAPABILITIES.map(({ wire }) => wire));

function requiredObject(value, label) {
  if (!value || typeof value !== 'object' || Array.isArray(value)) throw new TypeError(`${label} must be an object`);
  return value;
}

function abortError(reason) {
  const error = new Error('extension call aborted', { cause: reason });
  error.name = 'AbortError';
  return error;
}

/** GUI interaction frames are not protocol Snapshots and retain their own wire vocabulary. */
export function validateUiEvent(value) {
  const event = requiredObject(value, 'UI event');
  if (typeof event.pane_provider === 'string' && typeof event.slot === 'string') return value;
  if (typeof event.interaction === 'string') {
    if (!['invoke', 'submit', 'change', 'select', 'scroll', 'close', 'context', 'key', 'focus', 'pointer', 'drag', 'drop'].includes(event.interaction)
      || typeof event.trigger !== 'string' || !Number.isSafeInteger(event.node) || event.node < 0
      || typeof event.id !== 'string' || event.id.length === 0 || event.id.length > 4096
      || (event.slot !== undefined && typeof event.slot !== 'string')) throw new TypeError('UI interaction event is malformed');
    return value;
  }
  // Protocol-1 hosts used either a string event tag or an externally-tagged
  // event object. Keep that compatibility path typed and bounded.
  if (typeof event.event === 'string') {
    if (!Number.isSafeInteger(event.node) || event.node < 0 || typeof event.id !== 'string' || event.id.length > 4096) throw new TypeError('legacy UI event is malformed');
    return value;
  }
  if (event.event && typeof event.event === 'object' && !Array.isArray(event.event) && Object.keys(event.event).length === 1) {
    const body = requiredObject(Object.values(event.event)[0], 'legacy UI event body');
    if (!Number.isSafeInteger(body.node) || body.node < 0 || typeof body.id !== 'string' || body.id.length > 4096) throw new TypeError('legacy UI event is malformed');
    return value;
  }
  throw new TypeError('event is neither a protocol snapshot nor a known UI event');
}

/** Refusal returned by the host, with its stable machine-readable category. */
export class ExtensionError extends Error {
  constructor(failure) {
    const kind = failure?.error ?? 'failed';
    super(failure?.detail ?? failure?.call ?? `extension call ${kind}`);
    this.name = 'ExtensionError';
    this.kind = kind;
    this.capability = failure?.capability;
  }
}

/**
 * One connected extension.
 *
 * The host answers calls in order on one channel. A bounded FIFO correlates
 * those answers with promises without inventing request identifiers the wire
 * protocol does not carry.
 */
export class Session {
  #socket;
  #reader = new Reader();
  #onReply;
  #onRows;
  #onEventError;
  #onClose;
  #events = new Set();
  #topics = new Set();
  #eventTopics = new Map();
  #pending = [];
  #pings = new Map();
  #nextPing = 1;
  #limit;
  #timeout;
  #closed = false;
  #granted = [];
  #greeted;
  #ready;
  #rejectReady;
  #welcomed = false;
  #backpressured = false;
  #closing;
  #dataListener = (chunk) => this.#receive(chunk);
  #endListener = () => this.#ended();
  #drainListener = () => { this.#backpressured = false; };
  #closeListener = () => {
    this.#finish(new Error('extension host connection closed'));
    this.#socket.removeListener('data', this.#dataListener);
    this.#socket.removeListener('end', this.#endListener);
    this.#socket.removeListener('drain', this.#drainListener);
    this.#socket.removeListener('close', this.#closeListener);
    this.#socket.removeListener('error', this.#errorListener);
  };
  #errorListener = (error) => this.#finish(error);

  constructor(socket, {
    onReply = () => {}, onRows = () => {}, onEvent = () => {}, onEventError = () => {}, onClose = () => {}, pendingLimit = 64, timeout = 30_000,
  } = {}) {
    if (!Number.isSafeInteger(pendingLimit) || pendingLimit < 1) throw new RangeError('pendingLimit must be a positive integer');
    if (!Number.isFinite(timeout) || timeout <= 0) throw new RangeError('timeout must be positive');
    this.#socket = socket;
    this.#limit = pendingLimit;
    this.#timeout = timeout;
    // Resolved once the host has greeted us and we have answered. Nothing may
    // be sent before that: the host reads the first frame it receives as the
    // greeting, so a call written earlier is swallowed and every call after it
    // arrives one step out of order.
    this.#greeted = new Promise((resolve, reject) => {
      this.#ready = resolve;
      this.#rejectReady = reject;
    });
    this.#onReply = onReply;
    this.#onRows = onRows;
    this.#onEventError = onEventError;
    this.#onClose = onClose;
    this.#events.add(onEvent);
    socket.on('data', this.#dataListener);
    socket.on('end', this.#endListener);
    socket.on('drain', this.#drainListener);
    socket.on('close', this.#closeListener);
    socket.on('error', this.#errorListener);
  }

  /** Capabilities the host granted, known once the greeting arrives. */
  get granted() {
    return this.#granted;
  }

  /** Immutable exact wire capabilities negotiated with the host. */
  get grantedCapabilities() {
    return this.#granted;
  }

  /** Resolves when the handshake is complete and calls may be sent. */
  get ready() {
    return this.#greeted;
  }

  /** Opens the socket the host provided. */
  static connect(path = process.env[SOCKET], handlers = {}) {
    if (!path) throw new Error(`${SOCKET} is not set; an extension runs inside a workspace`);
    const connectTimeout = handlers.connectTimeout ?? 30_000;
    if (!Number.isFinite(connectTimeout) || connectTimeout <= 0) throw new RangeError('connectTimeout must be positive');
    return new Promise((resolve, reject) => {
      const socket = net.createConnection(path);
      let settled = false;
      const timer = setTimeout(() => {
        if (settled) return;
        settled = true;
        socket.destroy();
        reject(new Error(`extension host handshake timed out after ${connectTimeout}ms`));
      }, connectTimeout);
      const fail = (error) => {
        if (settled) return;
        settled = true;
        clearTimeout(timer);
        socket.destroy();
        reject(error);
      };
      socket.once('error', fail);
      // Resolve only after the handshake, so a caller that renders straight
      // away cannot outrun it.
      socket.once('connect', () => {
        const session = new Session(socket, handlers);
        session.ready.then(() => {
          if (settled) return session.close();
          settled = true;
          clearTimeout(timer);
          socket.removeListener('error', fail);
          resolve(session);
        }).catch(fail);
      });
    });
  }

  /** Sends one call and resolves with the tagged host reply. */
  call(name, argument, { signal } = {}) {
    if (this.#closed) return Promise.reject(new Error('extension session is closed'));
    if (!this.#welcomed) return Promise.reject(new Error('extension host handshake is not complete'));
    if (signal !== undefined && (typeof signal !== 'object' || typeof signal.addEventListener !== 'function'
      || typeof signal.removeEventListener !== 'function' || typeof signal.aborted !== 'boolean')) {
      return Promise.reject(new TypeError('call signal must be an AbortSignal'));
    }
    if (signal?.aborted) return Promise.reject(abortError(signal.reason));
    const capability = name === 'event_subscribe' || name === 'event_unsubscribe'
      ? TOPIC_CAPABILITIES.get(argument?.topic)
      : PROTOCOL_REQUEST_CAPABILITIES[name];
    if (capability === undefined) return Promise.reject(new RangeError(`unclassified extension request ${name}`));
    if (!this.#granted.includes(capability)) {
      return Promise.reject(new ExtensionError({
        error: 'denied', capability,
        detail: `extension lacks negotiated capability ${capability}`,
      }));
    }
    if (this.#pending.length >= this.#limit) {
      return Promise.reject(new Error(`extension call limit of ${this.#limit} is exhausted`));
    }
    if (this.#backpressured) return Promise.reject(new Error('extension socket is applying write backpressure'));
    const payload = encodeRequest(name, argument);
    return new Promise((resolve, reject) => {
      const abort = signal ? () => {
        const error = abortError(signal.reason);
        // Calls share one ordered channel without request identifiers. Once a
        // frame is written, retaining the session could bind its late reply to
        // the next caller, so cancellation is deliberately fail-closed.
        this.#finish(error);
        this.#socket.destroy();
      } : undefined;
      const timer = setTimeout(() => {
        const error = new Error(`extension call ${name} timed out after ${this.#timeout}ms`);
        // Without request identifiers, continuing after one missing ordered
        // reply could give every later caller the wrong answer. End the session.
        this.#finish(error);
        this.#socket.destroy();
      }, this.#timeout);
      this.#pending.push({ resolve, reject, timer, name, argument, signal, abort });
      signal?.addEventListener('abort', abort, { once: true });
      try {
        this.#write({ channel: CALLS, kind: KIND.request, payload });
      } catch (error) {
        clearTimeout(timer);
        this.#pending.pop();
        signal?.removeEventListener('abort', abort);
        reject(error);
      }
    });
  }

  /** Answers a row window the host asked for. */
  answer(channel, window) {
    this.#write({ channel, kind: KIND.response, payload: window });
  }

  /** Round-trips an opaque bounded heartbeat without consuming call ordering. */
  ping() {
    if (this.#closed) return Promise.reject(new Error('extension session is closed'));
    if (this.#backpressured) return Promise.reject(new Error('extension socket is applying write backpressure'));
    const token = Buffer.allocUnsafe(8);
    token.writeBigUInt64LE(BigInt(this.#nextPing++));
    const key = token.toString('hex');
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.#pings.delete(key);
        reject(new Error(`extension ping timed out after ${this.#timeout}ms`));
      }, this.#timeout);
      this.#pings.set(key, { resolve, reject, timer });
      try { this.#write({ channel: CONTROL, kind: KIND.ping, payload: token }); }
      catch (error) { clearTimeout(timer); this.#pings.delete(key); reject(error); }
    });
  }

  /** Adds a pushed-event observer and returns a synchronous disposer. */
  onEvent(listener) {
    if (typeof listener !== 'function') throw new TypeError('event listener must be a function');
    this.#events.add(listener);
    return () => this.#events.delete(listener);
  }

  close() {
    if (this.#closing) return this.#closing;
    this.#finish(new Error('extension session closed'));
    this.#closing = new Promise((resolve) => {
      if (this.#socket.destroyed) return resolve();
      const timer = setTimeout(() => {
        this.#socket.destroy();
        this.#detachDataListeners();
        resolve();
      }, Math.min(this.#timeout, CLOSE_TIMEOUT));
      timer.unref?.();
      this.#socket.end(encode({ channel: CONTROL, kind: KIND.close, payload: Buffer.alloc(0) }), () => {
        clearTimeout(timer);
        this.#socket.destroy();
        this.#detachDataListeners();
        resolve();
      });
    });
    return this.#closing;
  }

  #ended() {
    try { this.#reader.finish(); }
    catch (error) { this.#finish(error); this.#socket.destroy(); return; }
    this.#finish(new Error('extension host connection closed'));
  }

  #receive(chunk) {
    try {
      for (const frame of this.#reader.take(chunk)) this.#handle(frame);
    } catch (error) {
      this.#finish(error);
      this.#socket.destroy();
    }
  }

  #handle(frame) {
    if ((frame.flags & FLAG_END) === 0) throw new Error('fragmented extension frames are unsupported');
    if ((frame.flags & ERROR) !== 0 && frame.kind !== KIND.response) throw new Error('error flag is only valid on responses');
    if ((frame.flags & COALESCED) !== 0 && frame.kind !== KIND.event) throw new Error('coalesced flag is only valid on events');
    if (frame.kind === KIND.ping) {
      if (!this.#welcomed) throw new Error('host ping arrived before the greeting');
      this.#write({ channel: frame.channel, kind: KIND.pong, payload: frame.payload });
      return;
    }
    if (frame.kind === KIND.pong) {
      if (!this.#welcomed) throw new Error('host pong arrived before the greeting');
      const key = frame.payload.toString('hex');
      const pending = this.#pings.get(key);
      if (!pending) throw new Error('host returned an unknown ping token');
      this.#pings.delete(key); clearTimeout(pending.timer); pending.resolve();
      return;
    }
    if (frame.kind === KIND.reset) throw new Error(`extension host reset the session: ${frame.payload.toString('utf8')}`);
    if (frame.kind === KIND.close) {
      if (frame.channel === CONTROL) { this.#finish(new Error('extension host closed the session')); this.#socket.destroy(); }
      else {
        const topic = this.#eventTopics.get(frame.channel);
        this.#eventTopics.delete(frame.channel);
        if (topic !== undefined) this.#topics.delete(topic);
      }
      return;
    }
    if (frame.channel === CONTROL) return this.#greet(frame);
    // A row request is the one thing the host pushes rather than answers.
    if (frame.kind === KIND.event && frame.payload && frame.payload.range !== undefined) {
      return this.#onRows(frame.payload, frame.channel);
    }
    if (frame.kind === KIND.response && frame.channel === CALLS) {
      const pending = this.#pending[0];
      if (!pending) return this.#onReply(frame.payload);
      const payload = (frame.flags & ERROR) !== 0
        ? validateFailure(frame.payload)
        : validateReplyFor(pending.name, frame.payload);
      this.#pending.shift();
      clearTimeout(pending.timer);
      pending.signal?.removeEventListener('abort', pending.abort);
      if ((frame.flags & ERROR) !== 0) pending.reject(new ExtensionError(payload));
      else {
        if (pending.name === 'event_subscribe') this.#topics.add(pending.argument.topic);
        if (pending.name === 'event_unsubscribe') {
          this.#topics.delete(pending.argument.topic);
          for (const [channel, topic] of this.#eventTopics) if (topic === pending.argument.topic) this.#eventTopics.delete(channel);
        }
        pending.resolve(payload);
      }
      this.#onReply(payload);
      return;
    }
    if (frame.kind === KIND.event) {
      let payload = frame.payload;
      if (typeof payload?.snapshot === 'string') {
        payload = validateSnapshot(payload);
        const topic = SNAPSHOT_TOPICS.get(payload.snapshot);
        if (!topic || !this.#topics.has(topic)) throw new TypeError(`snapshot ${payload.snapshot} has no active subscription`);
        const bound = this.#eventTopics.get(frame.channel);
        if (bound !== undefined && bound !== topic) throw new TypeError(`event channel ${frame.channel} changed topic`);
        this.#eventTopics.set(frame.channel, topic);
      } else {
        payload = validateUiEvent(payload);
      }
      try {
        for (const listener of this.#events) {
          try {
            listener(payload, frame.channel);
          } catch (error) {
            try { this.#onEventError(error); } catch { /* Error reporting cannot strand event credit. */ }
          }
        }
      } finally {
        // Returning one credit after attempting every listener bounds a producer
        // without allowing one faulty observer to stall the whole event stream.
        this.#write({ channel: frame.channel, kind: KIND.credit, payload: 1 });
      }
      return;
    }
    throw new Error(`unexpected ${frame.kind} frame on channel ${frame.channel}`);
  }

  #write(frame) {
    if (this.#closed && frame.kind !== KIND.close) throw new Error('extension session is closed');
    if (this.#backpressured) throw new Error('extension socket is applying write backpressure');
    if (!this.#socket.write(encode(frame))) this.#backpressured = true;
  }

  #detachDataListeners() {
    this.#socket.removeListener('data', this.#dataListener);
    this.#socket.removeListener('end', this.#endListener);
    this.#socket.removeListener('drain', this.#drainListener);
  }

  #finish(error) {
    if (this.#closed) return;
    this.#closed = true;
    this.#rejectReady(error);
    for (const pending of this.#pending.splice(0)) {
      clearTimeout(pending.timer);
      pending.signal?.removeEventListener('abort', pending.abort);
      pending.reject(error);
    }
    for (const pending of this.#pings.values()) {
      clearTimeout(pending.timer);
      pending.reject(error);
    }
    this.#pings.clear();
    this.#events.clear();
    this.#topics.clear();
    this.#eventTopics.clear();
    try { this.#onClose(error); } catch { /* Lifecycle reporting cannot prevent closure. */ }
  }

  /** The host speaks first and states the grant, so an extension knows what it
   * holds before it asks for anything. */
  #greet(frame) {
    if (this.#welcomed) throw new Error('host sent a second greeting');
    if (frame.kind !== KIND.open) throw new Error('host greeting must open the control channel');
    const welcome = frame.payload;
    if (!welcome || welcome.protocol === undefined) return;
    if (welcome.protocol !== PROTOCOL) {
      throw new Error(`host speaks protocol ${welcome.protocol}, this extension speaks ${PROTOCOL}`);
    }
    const granted = welcome.granted ?? [];
    if (!Array.isArray(granted) || granted.some((capability) => typeof capability !== 'string' || !CAPABILITIES.has(capability))) {
      throw new TypeError('host greeting contains an unknown capability');
    }
    this.#granted = Object.freeze([...new Set(granted)]);
    this.#write(
      {
        channel: CONTROL,
        kind: KIND.response,
        payload: { protocol: PROTOCOL, name: welcome.peer ?? welcome.extension, features: [] },
      },
    );
    this.#welcomed = true;
    this.#ready();
  }
}
