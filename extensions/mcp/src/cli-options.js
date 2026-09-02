const WORKSPACE_BYTES = 128;
const SOCKET_BYTES = 4096;

const bounded = (flag, value, limit) => {
  if (value == null || value.length === 0 || value.trim() !== value || value.includes('\0')) {
    throw new TypeError(`${flag} requires one non-empty value without surrounding whitespace`);
  }
  if (new TextEncoder().encode(value).byteLength > limit) throw new RangeError(`${flag} exceeds ${limit} bytes`);
  return value;
};

export function parseCli(argv) {
  if (argv.length === 1 && argv[0] === '--help') return { help: true };
  const values = new Map();
  for (let index = 0; index < argv.length; index += 2) {
    const flag = argv[index];
    if (flag !== '--socket' && flag !== '--workspace') throw new TypeError(`unknown argument ${JSON.stringify(flag)}`);
    if (values.has(flag)) throw new TypeError(`${flag} may be provided only once`);
    values.set(flag, argv[index + 1]);
  }
  return {
    help: false,
    socket: bounded('--socket', values.get('--socket'), SOCKET_BYTES),
    workspace: bounded('--workspace', values.get('--workspace'), WORKSPACE_BYTES),
  };
}

export function assertWorkspace(hosting, expected) {
  if (hosting?.name !== expected) {
    throw new Error(`socket hosts workspace ${JSON.stringify(hosting?.name ?? null)}, expected ${JSON.stringify(expected)}`);
  }
}
