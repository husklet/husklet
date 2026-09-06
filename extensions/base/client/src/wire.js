// Framing, byte for byte as the host writes it.
//
// A fixed twelve byte header, little endian: payload length, channel, kind,
// flags, and two reserved bytes that must be zero. The host refuses anything
// else, so this file is the one place the layout is written down on this side.

import { Buffer } from 'node:buffer';

export const KIND = {
  request: 1,
  response: 2,
  event: 3,
  open: 4,
  close: 5,
  reset: 6,
  credit: 7,
  ping: 8,
  pong: 9,
};

export const FLAG_END = 1;
export const CONTROL = 0;
export const HEADER = 12;
/// Largest payload the host accepts. Anything above is refused before it is read.
export const PAYLOAD_LIMIT = 1 << 20;
const KNOWN_FLAGS = 0b0000_0111;
const KINDS = new Set(Object.values(KIND));
const UTF8 = new TextDecoder('utf-8', { fatal: true });
const CAPACITY = HEADER + PAYLOAD_LIMIT;

/** Encodes one frame. */
export function encode({ channel = CONTROL, kind, payload, flags = FLAG_END }) {
  const body = Buffer.isBuffer(payload) || payload instanceof Uint8Array
    ? Buffer.from(payload)
    : Buffer.from(JSON.stringify(payload), 'utf8');
  if (body.length > PAYLOAD_LIMIT) {
    throw new Error(`frame payload is ${body.length} bytes, above the ${PAYLOAD_LIMIT} limit`);
  }
  const frame = Buffer.allocUnsafe(HEADER + body.length);
  frame.writeUInt32LE(body.length, 0);
  frame.writeUInt32LE(channel, 4);
  frame.writeUInt8(kind, 8);
  frame.writeUInt8(flags, 9);
  frame.writeUInt16LE(0, 10);
  body.copy(frame, HEADER);
  return frame;
}

/**
 * Accumulates bytes and yields whole frames.
 *
 * A socket hands over arbitrary slices, so a frame arrives in pieces and
 * several arrive together; both are ordinary and neither is an error.
 */
export class Reader {
  #held = Buffer.alloc(0);

  /** Adds bytes and returns every frame they completed. */
  take(chunk) {
    if (!(chunk instanceof Uint8Array)) throw new TypeError('frame chunk must be bytes');
    const frames = [];
    let offset = 0;
    while (offset < chunk.length) {
      const room = CAPACITY - this.#held.length;
      if (room === 0) throw new Error(`frame exceeds the ${PAYLOAD_LIMIT} byte payload limit`);
      const part = chunk.subarray(offset, offset + room);
      this.#held = this.#held.length === 0 ? Buffer.from(part) : Buffer.concat([this.#held, part]);
      offset += part.length;
      for (;;) {
        const frame = this.#next();
        if (frame === null) break;
        frames.push(frame);
      }
    }
    return frames;
  }

  #next() {
    if (this.#held.length < HEADER) return null;
    const length = this.#held.readUInt32LE(0);
    if (length > PAYLOAD_LIMIT) {
      throw new Error(`frame declares ${length} bytes, above the ${PAYLOAD_LIMIT} limit`);
    }
    const kind = this.#held.readUInt8(8);
    const flags = this.#held.readUInt8(9);
    if (!KINDS.has(kind)) throw new Error(`frame has unknown kind ${kind}`);
    if ((flags & ~KNOWN_FLAGS) !== 0) throw new Error(`frame has unknown flags ${flags}`);
    if (this.#held.readUInt16LE(10) !== 0) throw new Error('frame reserved bytes must be zero');
    const total = HEADER + length;
    if (this.#held.length < total) return null;
    const body = this.#held.subarray(HEADER, total);
    let payload = Buffer.from(body);
    if (![KIND.ping, KIND.pong, KIND.close, KIND.reset].includes(kind)) {
      try {
        payload = JSON.parse(UTF8.decode(body));
      } catch (error) {
        throw new Error(`frame payload is not valid UTF-8 JSON: ${error.message}`, { cause: error });
      }
    }
    const frame = {
      channel: this.#held.readUInt32LE(4),
      kind,
      flags,
      payload,
    };
    this.#held = this.#held.subarray(total);
    return frame;
  }

  /** Refuses an EOF that cut a header or payload short. */
  finish() {
    if (this.#held.length !== 0) {
      const held = this.#held.length;
      this.#held = Buffer.alloc(0);
      throw new Error(`extension host closed with an unfinished frame (${held} bytes buffered)`);
    }
  }
}
