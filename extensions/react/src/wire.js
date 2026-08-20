// Framing, byte for byte as the host writes it.
//
// A fixed twelve byte header, little endian: payload length, channel, kind,
// flags, and two reserved bytes that must be zero. The host refuses anything
// else, so this file is the one place the layout is written down on this side.

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

/** Encodes one frame. */
export function encode({ channel = CONTROL, kind, payload, flags = FLAG_END }) {
  const body = Buffer.from(JSON.stringify(payload), 'utf8');
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
    this.#held = this.#held.length === 0 ? chunk : Buffer.concat([this.#held, chunk]);
    const frames = [];
    for (;;) {
      const frame = this.#next();
      if (frame === null) return frames;
      frames.push(frame);
    }
  }

  #next() {
    if (this.#held.length < HEADER) return null;
    const length = this.#held.readUInt32LE(0);
    if (length > PAYLOAD_LIMIT) {
      throw new Error(`frame declares ${length} bytes, above the ${PAYLOAD_LIMIT} limit`);
    }
    const total = HEADER + length;
    if (this.#held.length < total) return null;
    const frame = {
      channel: this.#held.readUInt32LE(4),
      kind: this.#held.readUInt8(8),
      flags: this.#held.readUInt8(9),
      payload: JSON.parse(this.#held.subarray(HEADER, total).toString('utf8')),
    };
    this.#held = this.#held.subarray(total);
    return frame;
  }
}
