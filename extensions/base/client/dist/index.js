export { ExtensionError, RowReplyMismatchError, RowRequestUnavailableError, Session, DATA, SOCKET, PROTOCOL, validateRowRequest, validateUiEvent, } from './session.js';
export { PROTOCOL_SPECIFICATION_VERSION, PROTOCOL_VERSION, PROTOCOL_BOUNDS, PROTOCOL_CAPABILITIES, PROTOCOL_TOPICS, PROTOCOL_REPLIES, PROTOCOL_REQUEST_CAPABILITIES, encodeRequest, validateRequest, validateReply, validateReplyFor, validateFailure, validateSnapshot, } from './generated-protocol.js';
import { semanticText, semanticXml } from './semantic.js';
export { semanticText, semanticXml };
import { ExtensionError, Session } from './session.js';
import { encodeRequest, PROTOCOL_REPLIES, PROTOCOL_REQUEST_CAPABILITIES, PROTOCOL_TOPICS, } from './generated-protocol.js';
/** A post-creation execution failure whose immutable identity remains recoverable. */
export class ExecutionOperationError extends Error {
    executionId;
    phase;
    execution;
    after;
    constructor(executionId, phase, cause, execution = undefined, after = undefined) {
        super(`execution ${executionId} ${phase} failed: ${cause instanceof Error ? cause.message : String(cause)}`);
        this.name = 'ExecutionOperationError';
        this.executionId = executionId;
        this.phase = phase;
        this.cause = cause;
        this.execution = execution;
        this.after = after;
    }
}
/** A client-owned execution exceeded its post-start wall-clock deadline. */
export class ExecutionDeadlineError extends Error {
    executionId;
    deadlineMs;
    constructor(executionId, deadlineMs) {
        super(`execution ${executionId} exceeded its ${deadlineMs}ms deadline`);
        this.name = 'ExecutionDeadlineError';
        this.executionId = executionId;
        this.deadlineMs = deadlineMs;
    }
}
/** Output retention advanced past the cursor, so a transcript/result would be incomplete. */
export class ExecutionOutputGapError extends Error {
    executionId;
    after;
    next;
    constructor(executionId, after, next) {
        super(`execution ${executionId} output has a gap after sequence ${after}`);
        this.name = 'ExecutionOutputGapError';
        this.executionId = executionId;
        this.after = after;
        this.next = next;
    }
}
/** Output reached EOF while the exact execution still reported itself running. */
export class ExecutionOutputEndedEarlyError extends Error {
    executionId;
    constructor(executionId) {
        super(`execution ${executionId} output reached EOF while the execution was still running`);
        this.name = 'ExecutionOutputEndedEarlyError';
        this.executionId = executionId;
    }
}
/** The host returned an internally inconsistent output page, so iteration cannot continue safely. */
export class ExecutionOutputProtocolError extends Error {
    executionId;
    after;
    next;
    constructor(executionId, after, next, detail) {
        super(`execution ${executionId} returned invalid output after sequence ${after}: ${detail}`);
        this.name = 'ExecutionOutputProtocolError';
        this.executionId = executionId;
        this.after = after;
        this.next = next;
    }
}
/** One bounded structured-output record was not valid JSON. */
export class JsonLineParseError extends SyntaxError {
    line;
    constructor(line, cause) {
        super(`execution output line ${line} is not valid JSON`);
        this.name = 'JsonLineParseError';
        this.line = line;
        this.cause = cause;
    }
}
/** One syntactically valid JSON record did not satisfy the consumer's result schema. */
export class JsonLineDecodeError extends TypeError {
    line;
    constructor(line, cause) {
        super(`execution output line ${line} does not match the expected schema`);
        this.name = 'JsonLineDecodeError';
        this.line = line;
        this.cause = cause;
    }
}
/** Durable JSON state could not be decoded; its exact identity remains available for CAS recovery. */
export class StateDecodeError extends TypeError {
    identity;
    constructor(identity, cause) {
        super(`extension state ${identity} could not be decoded`);
        this.name = 'StateDecodeError';
        this.identity = identity;
        this.cause = cause;
    }
}
/** Catalogue discovery was bounded before it became a complete searchable set. */
export class IncompleteCatalogueError extends Error {
    received;
    constructor(received) {
        super(`extension catalogue is incomplete after ${received} entries; no continuation is available`);
        this.name = 'IncompleteCatalogueError';
        this.received = received;
    }
}
function outputAbort(signal) {
    const error = new Error('execution output iteration aborted', { cause: signal?.reason });
    error.name = 'AbortError';
    return error;
}
function requireOutputActive(signal) {
    if (signal?.aborted)
        throw outputAbort(signal);
}
function exactExecutionSignal(signal) {
    if (typeof signal !== 'string' ||
        signal.length < 1 ||
        signal.length > 32 ||
        !/^[A-Za-z0-9+-]+$/.test(signal)) {
        throw new TypeError('execution signal must be a 1..32 byte ASCII signal name or number');
    }
    return signal;
}
function exactExecutionCancellation(timeoutMs) {
    if (!Number.isSafeInteger(timeoutMs) || timeoutMs < 1 || timeoutMs > 30_000) {
        throw new RangeError('execution cancellation timeout must be between 1 and 30000ms');
    }
    return timeoutMs;
}
function exactExecutionDeadline(deadlineMs) {
    if (deadlineMs !== undefined &&
        (!Number.isSafeInteger(deadlineMs) || deadlineMs < 1 || deadlineMs > 86_400_000)) {
        throw new RangeError('execution deadline must be an integer between 1 and 86400000ms');
    }
    return deadlineMs;
}
function exactExecutionPageLimit(limit) {
    if (!Number.isSafeInteger(limit) || limit < 1 || limit > 16) {
        throw new RangeError('execution output limit must be between 1 and 16');
    }
    return limit;
}
function exactExecutionLineLimit(limit) {
    if (limit !== undefined && (!Number.isSafeInteger(limit) || limit < 1 || limit > 1_000_000)) {
        throw new RangeError('execution maxLines must be an integer between 1 and 1000000');
    }
    return limit;
}
function exactExecutionOutputPage(page, limit, executionId, after) {
    if (page.entries.length > limit) {
        throw new TypeError('host returned an execution output page that exceeded its requested entry limit');
    }
    if (page.entries.some(({ stream }) => stream !== 'stdout' && stream !== 'stderr')) {
        throw new TypeError('host returned an execution output entry with an unknown stream');
    }
    if (!page.gap) {
        const sequences = page.entries.map((entry) => entry.sequence);
        const contiguous = sequences.every((sequence, index) => sequence === after + index + 1);
        const last = sequences.at(-1);
        let invalid;
        if (page.next < after)
            invalid = 'cursor moved backwards';
        else if (page.eof && page.more)
            invalid = 'page is both final and continued';
        else if (sequences.length === 0 && page.next !== after)
            invalid = 'empty page advanced its cursor';
        else if (sequences.length > 0 && (!contiguous || last !== page.next))
            invalid = 'entry sequence is not contiguous with its continuation cursor';
        else if (page.more && page.next === after)
            invalid = 'continued page did not advance its cursor';
        if (invalid)
            throw new ExecutionOutputProtocolError(executionId, after, page.next, invalid);
    }
    return page;
}
function exactExecutionPollInterval(pollIntervalMs) {
    if (!Number.isSafeInteger(pollIntervalMs) || pollIntervalMs < 10 || pollIntervalMs > 60_000) {
        throw new RangeError('execution output poll interval must be between 10 and 60000ms');
    }
    return pollIntervalMs;
}
function exactTerminalReadLines(lines) {
    if (lines !== undefined && (!Number.isSafeInteger(lines) || lines < 1 || lines > 2000)) {
        throw new TerminalReadLimitError(lines);
    }
    return lines;
}
function outputPoll(ms, signal) {
    requireOutputActive(signal);
    return new Promise((resolve, reject) => {
        const timer = setTimeout(done, ms);
        function done() {
            signal?.removeEventListener('abort', abort);
            resolve();
        }
        function abort() {
            clearTimeout(timer);
            reject(outputAbort(signal));
        }
        signal?.addEventListener('abort', abort, { once: true });
    });
}
function outputStep(step, signal) {
    requireOutputActive(signal);
    return new Promise((resolve, reject) => {
        let settled = false;
        const finish = (complete, value) => {
            if (settled)
                return;
            settled = true;
            signal?.removeEventListener('abort', abort);
            complete(value);
        };
        const abort = () => finish(reject, outputAbort(signal));
        signal?.addEventListener('abort', abort, { once: true });
        Promise.resolve()
            .then(step)
            .then((value) => finish(resolve, value), (error) => finish(reject, error));
    });
}
/** A terminal authority succeeded, but its bounded observation could not be completed. */
export class TerminalOperationError extends Error {
    operation;
    result;
    constructor(operation, result, cause) {
        super(`terminal ${operation} observation failed: ${cause instanceof Error ? cause.message : String(cause)}`);
        this.name = 'TerminalOperationError';
        this.operation = operation;
        this.result = Object.freeze({ ...result });
        this.cause = cause;
    }
}
/** A terminal text request cannot be represented by the host's bounded pane tail. */
export class TerminalReadLimitError extends RangeError {
    requested;
    maximum;
    constructor(requested, maximum = 2000) {
        super(`terminal text lines must be an integer between 1 and ${maximum}`);
        this.name = 'TerminalReadLimitError';
        this.requested = requested;
        this.maximum = maximum;
    }
}
/** A pane advanced or was replaced between discovery and its bounded text projection. */
export class PaneChangedError extends Error {
    slot;
    expected;
    observed;
    constructor(slot, expected, observed) {
        super(`pane ${slot} changed during bounded text conversion`);
        this.name = 'PaneChangedError';
        this.slot = slot;
        this.expected = Object.freeze({
            generation: expected.generation,
            revision: expected.revision,
        });
        this.observed = Object.freeze({
            generation: observed.generation,
            revision: observed.revision,
        });
    }
}
/** The pane layout kept changing while a bounded coherent inventory was assembled. */
export class PaneInventoryChangedError extends Error {
    attempts;
    before;
    after;
    constructor(attempts, before, after) {
        super(`pane inventory changed during ${attempts} bounded snapshot attempt${attempts === 1 ? '' : 's'}`);
        this.name = 'PaneInventoryChangedError';
        this.attempts = attempts;
        const cursor = ({ slot, generation, revision, focused }) => Object.freeze({ slot, generation, revision, focused });
        this.before = Object.freeze(before.panes.map(cursor));
        this.after = Object.freeze(after.panes.map(cursor));
    }
}
/** Bounded pane discovery omitted identities, so whole-layout stability cannot be proven. */
export class IncompletePaneInventoryError extends Error {
    panes;
    constructor(panes) {
        super(`stable pane inventory omitted additional panes after observing ${panes.length} identities`);
        this.name = 'IncompletePaneInventoryError';
        this.panes = Object.freeze(panes.map(({ slot, generation, revision }) => Object.freeze({ slot, generation, revision })));
    }
}
/** A requested pane is absent or cannot be resolved from a bounded inventory. */
export class PaneUnavailableError extends Error {
    slot;
    reason;
    constructor(slot, reason) {
        const detail = reason === 'inventory-truncated'
            ? 'pane cannot be resolved from a truncated inventory'
            : 'pane does not exist';
        super(`${detail}: ${slot}`);
        this.name = 'PaneUnavailableError';
        this.slot = slot;
        this.reason = reason;
    }
}
/** Filesystem history rotated before an incremental consumer could resume its cursor. */
export class FilesystemJournalGapError extends Error {
    requested;
    replacement;
    constructor(requested, replacement) {
        super(`filesystem journal ${requested.journal} has no history after revision ${requested.revision}`);
        this.name = 'FilesystemJournalGapError';
        this.requested = Object.freeze({ ...requested });
        this.replacement = Object.freeze({ ...replacement });
    }
}
/** A directory page crossed generations and enumeration must restart from its root. */
export class DirectoryIdentityChangedError extends Error {
    path;
    expected;
    actual;
    after;
    constructor(path, expected, actual, after) {
        super(`filesystem directory ${path} changed identity from ${expected} to ${actual}`);
        this.name = 'DirectoryIdentityChangedError';
        this.path = path;
        this.expected = expected;
        this.actual = actual;
        this.after = after;
    }
}
/** One exact file generation could not be decoded as UTF-8. */
export class FileTextDecodeError extends TypeError {
    path;
    identity;
    bytes;
    constructor(path, identity, bytes, cause) {
        super(`filesystem text ${path} at identity ${identity} is not valid UTF-8`, { cause });
        this.name = 'FileTextDecodeError';
        this.path = path;
        this.identity = identity;
        this.bytes = bytes;
    }
}
/** One exact file generation exceeded the caller-owned text collection bound. */
export class FileTextLimitError extends RangeError {
    path;
    identity;
    total;
    limit;
    constructor(path, identity, total, limit) {
        super(`filesystem text ${path} at identity ${identity} exceeds the caller's ${limit} byte limit`);
        this.name = 'FileTextLimitError';
        this.path = path;
        this.identity = identity;
        this.total = total;
        this.limit = limit;
    }
}
/** A ranged read crossed file generations and must be restarted from a coherent identity. */
export class FileIdentityChangedError extends Error {
    path;
    expected;
    actual;
    offset;
    constructor(path, expected, actual, offset) {
        super(`filesystem file ${path} changed identity from ${expected} to ${actual} at offset ${offset}`);
        this.name = 'FileIdentityChangedError';
        this.path = path;
        this.expected = expected;
        this.actual = actual;
        this.offset = offset;
    }
}
/** One identity reported contradictory file extents across ranged reads. */
export class FileExtentChangedError extends Error {
    path;
    identity;
    expectedTotal;
    actualTotal;
    offset;
    constructor(path, identity, expectedTotal, actualTotal, offset) {
        super(`filesystem file ${path} at identity ${identity} changed total from ${expectedTotal} to ${actualTotal} bytes at offset ${offset}`);
        this.name = 'FileExtentChangedError';
        this.path = path;
        this.identity = identity;
        this.expectedTotal = expectedTotal;
        this.actualTotal = actualTotal;
        this.offset = offset;
    }
}
/** Reference-counted host subscriptions, keyed by session and snapshot topic. */
const subscriptions = new WeakMap();
const SNAPSHOT_TOPICS = Object.freeze([
    'containers',
    'container-inventory',
    'executions',
    'images',
    'image-pulls',
    'volumes',
    'networks',
    'terminal',
    'pane-changes',
    'extensions',
    'extension-acquisitions',
    'workspace-lifecycle',
    'workspace-events',
    'filesystem',
]);
function immutableIdentity(id, widths, noun) {
    if (typeof id === 'string' && widths.includes(id.length) && /^[0-9a-f]+$/.test(id))
        return id;
    throw new TypeError(`${noun} operation requires the complete immutable ID returned by inspection`);
}
function exactAcquisitionJob(job) {
    if (typeof job !== 'string' ||
        job.length === 0 ||
        job.includes('\0') ||
        new TextEncoder().encode(job).byteLength > 128)
        throw new TypeError('extension acquisition requires a 1..128 byte NUL-free job identity');
    return job;
}
function exactAcquisitionStatus(job, status) {
    const states = new Set([
        'inspecting',
        'pulling',
        'reading-manifest',
        'ready',
        'committing',
        'installed',
        'updated',
        'failed',
        'cancelled',
    ]);
    const progress = status.progress ?? null;
    const candidate = status.candidate ?? null;
    const error = status.error ?? null;
    const validProgress = status.state === 'pulling' &&
        progress !== null &&
        new TextEncoder().encode(progress.status).byteLength <= 512 &&
        !progress.status.includes('\0') &&
        (progress.id === null ||
            (!progress.id.includes('\0') && new TextEncoder().encode(progress.id).byteLength <= 512)) &&
        (progress.current === null || progress.total === null || progress.current <= progress.total);
    if (status.job !== job ||
        !states.has(status.state) ||
        status.reference.length === 0 ||
        status.reference.includes('\0') ||
        new TextEncoder().encode(status.reference).byteLength > 512 ||
        (status.state === 'pulling' ? !validProgress : progress !== null) ||
        (status.state === 'ready' ? candidate === null : candidate !== null) ||
        (status.state === 'failed' ? error === null : error !== null))
        throw new TypeError('host returned an inconsistent extension acquisition status');
    if (candidate)
        immutableDigest(candidate.image_digest, 'extension candidate image');
    return {
        ...status,
        state: status.state,
        progress,
        candidate,
        error,
    };
}
function extensionAuthority(extension) {
    return JSON.stringify({
        granted: extension.granted ?? null,
        containers: extension.containers ?? null,
        images: extension.images ?? null,
        networks: extension.networks ?? null,
        volumes: extension.volumes ?? null,
        filesystem: extension.filesystem ?? null,
        workspace_environment: extension.workspace_environment ?? null,
        pane_providers: extension.pane_providers ?? null,
    });
}
function exactFileRange(offset, limit) {
    if (!Number.isSafeInteger(offset) || offset < 0) {
        throw new RangeError('filesystem range offset must be a nonnegative safe integer');
    }
    if (!Number.isSafeInteger(limit) || limit < 1 || limit > 65_536) {
        throw new RangeError('filesystem range limit must be an integer between 1 and 65536');
    }
    return [offset, limit];
}
function exactFileContents(contents) {
    if (contents === null ||
        contents === undefined ||
        typeof contents[Symbol.iterator] !== 'function')
        throw new TypeError('filesystem contents must be an iterable of bytes');
    const bounded = [];
    for (const byte of contents) {
        if (!Number.isInteger(byte) || byte < 0 || byte > 255)
            throw new TypeError('filesystem contents must contain bytes from 0 through 255');
        if (bounded.length === 65_536)
            throw new RangeError('filesystem writes are limited to 65536 bytes');
        bounded.push(byte);
    }
    return bounded;
}
function exactFilesystemPageSize(limit) {
    if (!Number.isSafeInteger(limit) || limit < 1 || limit > 256) {
        throw new RangeError('filesystem page size must be an integer between 1 and 256');
    }
    return limit;
}
function exactFilesystemJournal(journal) {
    if (typeof journal !== 'string' || !/^[0-9a-fA-F]{32}$/.test(journal)) {
        throw new TypeError('filesystem journal identity must be 32 hexadecimal characters');
    }
    return journal;
}
function filesystemParts(path) {
    return path.split(/[/\\]/).filter((part) => part !== '' && part !== '.');
}
function filesystemSelectorPermits(selector, path) {
    if ('exact' in selector)
        return selector.exact === path;
    const root = filesystemParts(selector.subtree);
    const candidate = filesystemParts(path);
    return candidate.length >= root.length && root.every((part, index) => candidate[index] === part);
}
function compareUtf8(left, right) {
    const encoder = new TextEncoder();
    const leftBytes = encoder.encode(left);
    const rightBytes = encoder.encode(right);
    const length = Math.min(leftBytes.length, rightBytes.length);
    for (let index = 0; index < length; index += 1) {
        if (leftBytes[index] !== rightBytes[index])
            return leftBytes[index] - rightBytes[index];
    }
    return leftBytes.length - rightBytes.length;
}
function filesystemAbort(signal) {
    const error = new Error('filesystem iteration aborted', { cause: signal?.reason });
    error.name = 'AbortError';
    return error;
}
function requireFilesystemActive(signal) {
    if (signal?.aborted)
        throw filesystemAbort(signal);
}
function requireStateUpdateActive(signal) {
    if (!signal?.aborted)
        return;
    const error = new Error('extension state update aborted', { cause: signal.reason });
    error.name = 'AbortError';
    throw error;
}
function isDirectFilesystemChild(parent, child) {
    const parts = (value) => value.split(/[\\/]/).filter((part) => part.length > 0 && part !== '.');
    const parentParts = parts(parent);
    const childParts = parts(child);
    return (childParts.length === parentParts.length + 1 &&
        parentParts.every((part, index) => childParts[index] === part));
}
function exactContainerName(name) {
    if (typeof name === 'string' && /^[A-Za-z0-9][A-Za-z0-9_.-]{0,127}$/.test(name))
        return name;
    throw new TypeError('container name must contain 1..128 ASCII letters, digits, underscores, periods, or hyphens and start with a letter or digit');
}
function endpointAliases(options) {
    if (options === undefined)
        return [];
    if (options === null ||
        typeof options !== 'object' ||
        Array.isArray(options) ||
        Object.keys(options).some((key) => key !== 'aliases')) {
        throw new TypeError('network connect options may contain only aliases');
    }
    if (options.aliases !== undefined && !Array.isArray(options.aliases)) {
        throw new TypeError('network endpoint aliases must be an array');
    }
    const aliases = options.aliases === undefined ? [] : [...options.aliases];
    if (aliases.length > 64 ||
        new Set(aliases).size !== aliases.length ||
        aliases.some((alias) => typeof alias !== 'string' ||
            alias.length < 1 ||
            alias.length > 253 ||
            !/^[A-Za-z0-9][A-Za-z0-9_.-]*$/.test(alias))) {
        throw new TypeError('network endpoint aliases must be at most 64 unique, 1..=253-byte ASCII endpoint names');
    }
    return aliases;
}
function exactPaneTitle(title) {
    if (typeof title === 'string' &&
        title.trim().length > 0 &&
        new TextEncoder().encode(title).byteLength <= 256 &&
        // eslint-disable-next-line no-control-regex
        !/[\u0000-\u001f\u007f-\u009f]/u.test(title))
        return title;
    throw new TypeError('pane title must be nonblank and contain at most 256 UTF-8 bytes without control characters');
}
function exactPaneRatio(ratio) {
    if (typeof ratio !== 'number' || !Number.isFinite(ratio) || ratio < 0.05 || ratio > 0.95) {
        throw new RangeError('terminal pane ratio must be a finite number within 0.05..=0.95');
    }
    return ratio;
}
function paneRatio(node, slot) {
    if (!node || node.kind !== 'split')
        return null;
    if (node.first?.kind === 'pane' && node.first.pane?.slot === slot)
        return node.ratio_per_mille / 1000;
    if (node.second?.kind === 'pane' && node.second.pane?.slot === slot)
        return 1 - node.ratio_per_mille / 1000;
    return paneRatio(node.first, slot) ?? paneRatio(node.second, slot);
}
function exactOccupantTarget(target) {
    const terminal = target?.kind === 'terminal' && Object.keys(target).length === 1;
    const name = (value) => typeof value === 'string' && value.length <= 64 && /^[a-z0-9][a-z0-9._-]*$/.test(value);
    const surface = target?.kind === 'surface' &&
        name(target.extension) &&
        name(target.provider) &&
        Object.keys(target).length === 3;
    if (!terminal && !surface)
        throw new TypeError('pane occupant target must be terminal or an exact extension/provider surface');
    return { ...target };
}
function exactSemanticAction(action) {
    if (!Number.isSafeInteger(action?.generation) ||
        action.generation < 0 ||
        !Number.isSafeInteger(action?.revision) ||
        action.revision < 0 ||
        !Number.isSafeInteger(action?.node) ||
        action.node < 0) {
        throw new TypeError('pane semantic action requires nonnegative safe integer generation, revision, and node');
    }
    if (action?.value != null && new TextEncoder().encode(action.value).byteLength > 4096) {
        throw new RangeError('pane semantic action value exceeds 4096 bytes');
    }
    return action;
}
function exactCommand(command) {
    if (!Array.isArray(command) ||
        command.length < 1 ||
        command.length > 64 ||
        command[0] === '' ||
        command.some((argument) => typeof argument !== 'string' ||
            new TextEncoder().encode(argument).byteLength > 4096 ||
            argument.includes('\0')) ||
        command.reduce((bytes, argument) => bytes + new TextEncoder().encode(argument).byteLength, 0) >
            32768) {
        throw new TypeError('command must contain 1..64 NUL-free arguments, each at most 4096 bytes and 32768 bytes in aggregate');
    }
    return command;
}
function exactExecEnvironment(environment = []) {
    if (!Array.isArray(environment) || environment.length > 256) {
        throw new TypeError('environment must contain at most 256 [name, value] pairs');
    }
    const encoder = new TextEncoder();
    let aggregate = 0;
    const names = new Set();
    for (const pair of environment) {
        if (!Array.isArray(pair) ||
            pair.length !== 2 ||
            pair.some((value) => typeof value !== 'string')) {
            throw new TypeError('environment entries must be [name, value] string pairs');
        }
        const [name, value] = pair;
        const nameBytes = encoder.encode(name).byteLength;
        const valueBytes = encoder.encode(value).byteLength;
        aggregate += nameBytes + valueBytes;
        if (!name ||
            nameBytes > 256 ||
            name.includes('=') ||
            name.includes('\0') ||
            valueBytes > 8192 ||
            value.includes('\0') ||
            names.has(name)) {
            throw new TypeError('environment names must be unique, nonempty, NUL-free, exclude =, and fit 256 bytes; values must be NUL-free and fit 8192 bytes');
        }
        names.add(name);
    }
    if (aggregate > 65536)
        throw new RangeError('environment exceeds 65536 UTF-8 bytes');
    return environment;
}
function exactExecCredentials(credentials, environment) {
    if (!Array.isArray(credentials) || credentials.length > 64) {
        throw new TypeError('credential environment must contain at most 64 [variable, key] pairs');
    }
    const normalized = credentials.map((pair) => {
        if (!Array.isArray(pair) ||
            pair.length !== 2 ||
            pair.some((value) => typeof value !== 'string')) {
            throw new TypeError('credential environment entries must be [variable, key] string pairs');
        }
        return [pair[0], exactCredentialKey(pair[1])];
    });
    exactExecEnvironment([...environment, ...normalized.map(([variable]) => [variable, ''])]);
    return normalized;
}
function containerMutation(reference, generation) {
    if (typeof reference !== 'string' ||
        reference.length === 0 ||
        reference.length > 128 ||
        reference.includes('\0')) {
        throw new TypeError('container mutation requires a nonempty NUL-free reference of at most 128 characters');
    }
    if (!Number.isSafeInteger(generation) || generation < 0) {
        throw new TypeError('container mutation requires an observed nonnegative safe generation');
    }
    return { id: reference, generation };
}
function exactPaneInput(input) {
    if (typeof input === 'string') {
        const bytes = new TextEncoder().encode(input);
        if (bytes.byteLength > 64 * 1024)
            throw new RangeError('terminal input exceeds the 65536 byte limit');
        return bytes;
    }
    const values = Array.from(input ?? []);
    if (values.length > 64 * 1024)
        throw new RangeError('terminal input exceeds the 65536 byte limit');
    if (values.some((value) => !Number.isInteger(value) || value < 0 || value > 255)) {
        throw new TypeError('terminal input bytes must be integers from 0 through 255');
    }
    return Uint8Array.from(values);
}
function exactExecutionInput(input) {
    if (typeof input === 'string') {
        const bytes = new Uint8Array(64 * 1024 + 1);
        const encoded = new TextEncoder().encodeInto(input, bytes);
        if (encoded.read !== input.length || encoded.written === 0 || encoded.written > 64 * 1024) {
            throw new RangeError('execution stdin chunks must contain between 1 and 65536 UTF-8 bytes');
        }
        return bytes.subarray(0, encoded.written);
    }
    const values = [];
    for (const value of input ?? []) {
        if (values.length === 64 * 1024)
            throw new RangeError('execution stdin chunks must contain between 1 and 65536 bytes');
        if (!Number.isInteger(value) || value < 0 || value > 255) {
            throw new TypeError('execution stdin bytes must be integers from 0 through 255');
        }
        values.push(value);
    }
    if (values.length === 0)
        throw new RangeError('execution stdin chunks must contain between 1 and 65536 bytes');
    return Uint8Array.from(values);
}
function exactStateBytes(input) {
    const values = [];
    for (const value of input ?? []) {
        if (values.length === 1024 * 1024)
            throw new RangeError('extension state exceeds the 1 MiB limit');
        if (!Number.isInteger(value) || value < 0 || value > 255)
            throw new TypeError('extension state bytes must be integers from 0 through 255');
        values.push(value);
    }
    return values;
}
function exactStateIdentity(identity) {
    if (identity === 'absent' || /^sha256:[0-9a-f]{64}$/.test(identity))
        return identity;
    throw new TypeError('extension state mutation requires the exact identity returned by state.read()');
}
function exactCredentialKey(key) {
    if (!/^[A-Za-z0-9._-]{1,64}$/.test(key))
        throw new TypeError("credential keys must be 1 through 64 ASCII letters, digits, '.', '_' or '-'");
    return key;
}
function exactCredentialBytes(input) {
    const values = [];
    for (const value of input ?? []) {
        if (values.length === 64 * 1024)
            throw new RangeError('credentials are limited to 64 KiB');
        if (!Number.isInteger(value) || value < 0 || value > 255)
            throw new TypeError('credential bytes must be integers from 0 through 255');
        values.push(value);
    }
    return values;
}
function exactStateCodec(codec) {
    if (typeof codec?.decode !== 'function' || typeof codec?.encode !== 'function')
        throw new TypeError('extension state JSON codec requires encode and decode functions');
    return codec;
}
function decodeJsonState(state, codec) {
    try {
        const bytes = Uint8Array.from(state.contents);
        const encoded = new TextDecoder('utf-8', { fatal: true }).decode(bytes);
        const value = encoded.length === 0 ? undefined : JSON.parse(encoded);
        return { identity: state.identity, value: codec.decode(value) };
    }
    catch (cause) {
        throw new StateDecodeError(state.identity, cause);
    }
}
function encodeJsonState(value, codec) {
    const encoded = JSON.stringify(exactStateCodec(codec).encode(value));
    if (encoded === undefined)
        throw new TypeError('extension state codec produced no JSON value');
    return exactStateBytes(new TextEncoder().encode(encoded));
}
function exactExecutionWaitOptions({ timeoutMs = 30_000, stdout = true, stderr = true } = {}) {
    if (!Number.isSafeInteger(timeoutMs) || timeoutMs < 1 || timeoutMs > 30_000) {
        throw new RangeError('execution wait timeout must be an integer from 1 through 30000 milliseconds');
    }
    if (typeof stdout !== 'boolean' || typeof stderr !== 'boolean' || (!stdout && !stderr)) {
        throw new TypeError('execution output requires at least one boolean stdout or stderr stream');
    }
    return { timeoutMs, stdout, stderr };
}
function immutableDigest(value, noun) {
    if (!/^sha256:[0-9a-f]{64}$/.test(value))
        throw new TypeError(`${noun} removal requires the complete immutable sha256 digest returned by inventory`);
    return value;
}
export async function connect(options = {}) {
    return Session.connect(options.path, options);
}
/**
 * Opens a surface and paints a dependency-free first frame.
 *
 * Extensions can do this before importing React or another renderer, keeping
 * cold-start feedback independent of framework initialization. Pass the
 * returned token to the renderer so it continues the same frame sequence.
 */
export async function bootstrapSurface(session, { title = 'Extension', label = 'Loading…', primary = false } = {}) {
    if (typeof title !== 'string' || title.trim().length === 0)
        throw new TypeError('surface title must be nonblank');
    if (typeof label !== 'string' || new TextEncoder().encode(label).byteLength > 4096) {
        throw new TypeError('surface label must be a string of at most 4096 UTF-8 bytes');
    }
    if (typeof primary !== 'boolean')
        throw new TypeError('surface primary must be boolean');
    let slot = '';
    if (!primary) {
        const opened = await session.call('interface_open_tab', { title });
        if (opened?.reply !== 'identity' ||
            typeof opened.with !== 'string' ||
            opened.with.length === 0) {
            throw new Error(`host replied ${opened?.reply ?? 'without a tag'}, expected identity`);
        }
        slot = opened.with;
    }
    const node = 1;
    const frame = {
        sequence: 1,
        patches: [
            { Create: { id: node, tag: 'Text' } },
            { SetProp: { id: node, prop: 'Label', value: { Text: label } } },
            { Insert: { parent: 0, child: node, before: null } },
        ],
    };
    try {
        const rendered = primary
            ? await session.call('interface_render', { frame })
            : await session.call('interface_render_at', { slot, frame });
        if (rendered?.reply !== 'done')
            throw new Error(`host replied ${rendered?.reply ?? 'without a tag'}, expected done`);
    }
    catch (error) {
        if (!primary)
            void session.call('interface_withdraw', { slot }).catch(() => { });
        throw error;
    }
    return Object.freeze({ slot, sequence: 1, nextNode: 2, bootstrapNode: node });
}
export function workspace(session, { signal } = {}) {
    const hostSession = session;
    if (signal !== undefined) {
        session = new Proxy(hostSession, {
            get(target, property) {
                if (property === 'call')
                    return (name, argument) => target.call(name, argument, { signal });
                const value = Reflect.get(target, property, target);
                return typeof value === 'function' ? value.bind(target) : value;
            },
        });
    }
    const expect = (reply, kind) => {
        if (reply?.reply !== kind)
            throw new Error(`host replied ${reply?.reply ?? 'without a tag'}, expected ${kind}`);
        return ('with' in reply ? reply.with : undefined);
    };
    const exactPane = (snapshot, slot, description) => {
        if (snapshot.slot !== slot) {
            throw new Error(`host returned ${description} for pane ${snapshot.slot}, expected ${slot}; no pane state was assumed`);
        }
        return snapshot;
    };
    const exactPaneInventory = (inventory) => {
        if (new Set(inventory.panes.map(({ slot }) => slot)).size !== inventory.panes.length) {
            throw new TypeError('host returned duplicate pane slot identities; no pane selection was assumed');
        }
        return inventory;
    };
    const exactTabs = (tabs) => {
        if (new Set(tabs.map(({ id }) => id)).size !== tabs.length) {
            throw new TypeError('host returned duplicate tab identities; no tab selection was assumed');
        }
        return tabs;
    };
    const exactImages = (images) => {
        if (new Set(images.map(({ id }) => id)).size !== images.length) {
            throw new TypeError('host returned duplicate immutable image identities; no image selection was assumed');
        }
        return images;
    };
    const exactNetworks = (networks) => {
        if (new Set(networks.map(({ id }) => id)).size !== networks.length) {
            throw new TypeError('host returned duplicate immutable network identities; no network selection was assumed');
        }
        return networks;
    };
    const exactVolumes = (volumes) => {
        if (new Set(volumes.map(({ name }) => name)).size !== volumes.length) {
            throw new TypeError('host returned duplicate volume identities; no volume selection was assumed');
        }
        return volumes;
    };
    const exactWorkspaceConfiguration = (configuration, name) => {
        if (configuration.name !== name) {
            throw new TypeError(`host returned workspace configuration for ${configuration.name}, expected ${name}; no workspace state was assumed`);
        }
        return configuration;
    };
    const exactTopology = (topology) => {
        exactTabs(topology.tabs);
        if (topology.active_tab != null &&
            !topology.tabs.some(({ id }) => id === topology.active_tab)) {
            throw new TypeError('host returned a terminal topology with an unknown active tab');
        }
        const slots = new Set();
        const visit = (node) => {
            if (node.kind === 'split') {
                visit(node.first);
                visit(node.second);
            }
            else if (slots.has(node.pane.slot)) {
                throw new TypeError('host returned duplicate pane slots in terminal topology; no layout selection was assumed');
            }
            else {
                slots.add(node.pane.slot);
            }
        };
        for (const tab of topology.tabs)
            visit(tab.root);
        return topology;
    };
    const exactExecution = (execution, id, operation) => {
        if (execution.id !== id) {
            throw new TypeError(`host returned ${operation} for execution ${execution.id}, expected ${id}; no execution state was assumed`);
        }
        return execution;
    };
    const done = async (name, argument) => expect(await session.call(name, argument), 'done');
    const requireCapabilities = (...capabilities) => {
        for (const capability of capabilities) {
            if (!session.grantedCapabilities.includes(capability)) {
                throw new ExtensionError({
                    error: 'denied',
                    capability,
                    detail: `extension lacks negotiated capability ${capability}`,
                });
            }
        }
    };
    const subscription = (call, topic) => {
        if (!SNAPSHOT_TOPICS.includes(topic))
            throw new RangeError(`host does not publish the ${topic} snapshot topic`);
        return done(call, { topic });
    };
    const states = subscriptions.get(hostSession) ?? new Map();
    subscriptions.set(hostSession, states);
    const subscribe = async (topic) => {
        if (!SNAPSHOT_TOPICS.includes(topic))
            throw new RangeError(`host does not publish the ${topic} snapshot topic`);
        let state = states.get(topic);
        if (!state) {
            state = { references: 0, active: false, operation: Promise.resolve() };
            states.set(topic, state);
        }
        state.references += 1;
        const operation = state.operation.then(async () => {
            if (!state.active) {
                await subscription('event_subscribe', topic);
                state.active = true;
            }
        });
        state.operation = operation.catch(() => { });
        try {
            await operation;
        }
        catch (error) {
            state.references -= 1;
            if (state.references === 0 && !state.active)
                states.delete(topic);
            throw error;
        }
    };
    const unsubscribe = async (topic) => {
        if (!SNAPSHOT_TOPICS.includes(topic))
            throw new RangeError(`host does not publish the ${topic} snapshot topic`);
        const state = states.get(topic);
        if (!state || state.references === 0)
            return;
        state.references -= 1;
        const operation = state.operation.then(async () => {
            if (state.references === 0 && state.active) {
                await expect(await hostSession.call('event_unsubscribe', { topic }), 'done');
                state.active = false;
            }
            if (state.references === 0 && !state.active)
                states.delete(topic);
        });
        state.operation = operation.catch(() => { });
        await operation;
    };
    const api = {
        get granted() {
            return session.grantedCapabilities ?? session.granted;
        },
        get grantedCapabilities() {
            return session.grantedCapabilities ?? session.granted;
        },
        get grantedFilesystem() {
            return session.grantedFilesystem;
        },
        get grantedContainers() {
            return session.grantedContainers;
        },
        get grantedImages() {
            return session.grantedImages;
        },
        get grantedNetworks() {
            return session.grantedNetworks;
        },
        get grantedVolumes() {
            return session.grantedVolumes;
        },
        get grantedWorkspaceEnvironment() {
            return session.grantedWorkspaceEnvironment;
        },
        info: async () => expect(await session.call('workspace_info'), 'workspace'),
        list: async () => {
            const workspaces = expect(await session.call('workspace_list'), 'workspaces');
            if (new Set(workspaces.map(({ name }) => name)).size !== workspaces.length) {
                throw new TypeError('host returned duplicate workspace identities; no workspace selection was assumed');
            }
            return workspaces;
        },
        inspect: async (name) => exactWorkspaceConfiguration(expect(await session.call('workspace_inspect', { name }), 'workspace_configuration'), name),
        create: async (configuration) => exactWorkspaceConfiguration(expect(await session.call('workspace_create', { configuration }), 'workspace_configuration'), configuration.name),
        update: async (name, generation, configurationRevision, configuration) => exactWorkspaceConfiguration(expect(await session.call('workspace_update', {
            name,
            generation: immutableIdentity(generation, [32], 'workspace generation'),
            configuration_revision: immutableIdentity(configurationRevision, [32], 'workspace configuration revision'),
            configuration,
        }), 'workspace_configuration'), name),
        patchEnvironment: async (name, generation, configurationRevision, patch) => expect(await session.call('workspace_environment_patch', {
            name,
            generation: immutableIdentity(generation, [32], 'workspace generation'),
            configuration_revision: immutableIdentity(configurationRevision, [32], 'workspace configuration revision'),
            patch,
        }), 'workspace_environment_patch'),
        delete: (name, generation) => done('workspace_delete', {
            name,
            generation: immutableIdentity(generation, [32], 'workspace generation'),
        }),
        start: (name) => done('workspace_start', { name }),
        stop: (name) => done('workspace_stop', { name }),
        restart: (name) => done('workspace_restart', { name }),
        notifications: {
            publish: (notification) => {
                if (!notification || typeof notification !== 'object')
                    throw new TypeError('notification must be an object');
                for (const [field, limit] of [
                    ['id', 128],
                    ['title', 256],
                    ['body', 4096],
                ]) {
                    const value = notification[field];
                    if (typeof value !== 'string' ||
                        value.length === 0 ||
                        new TextEncoder().encode(value).byteLength > limit ||
                        // eslint-disable-next-line no-control-regex
                        /[\u0000-\u001f\u007f]/u.test(value)) {
                        throw new TypeError(`notification ${field} must contain 1..${limit} UTF-8 bytes without control characters`);
                    }
                }
                return done('notification_publish', { notification });
            },
        },
        extensions: {
            list: async () => {
                const extensions = expect(await session.call('extension_list'), 'extensions');
                if (new Set(extensions.map(({ name }) => name)).size !== extensions.length) {
                    throw new TypeError('host returned duplicate extension identities; no extension selection was assumed');
                }
                return extensions;
            },
            catalogue: async () => {
                const catalogue = expect(await session.call('extension_catalogue'), 'extension_catalogue');
                if (catalogue.entries.length > 64)
                    throw new TypeError('host returned more than 64 extension catalogue entries');
                return catalogue;
            },
            requireCompleteCatalogue: async () => {
                const catalogue = await api.extensions.catalogue();
                if (!catalogue.complete)
                    throw new IncompleteCatalogueError(catalogue.entries.length);
                return catalogue;
            },
            inspect: async (name) => {
                const extension = expect(await session.call('extension_inspect', { name }), 'extension');
                if (extension.name !== name) {
                    throw new TypeError(`host returned extension ${extension.name}, expected ${name}; no extension state was assumed`);
                }
                return extension;
            },
            enable: (name, imageDigest) => done('extension_enable', {
                name,
                image_digest: immutableDigest(imageDigest, 'extension image'),
            }),
            disable: (name, imageDigest) => done('extension_disable', {
                name,
                image_digest: immutableDigest(imageDigest, 'extension image'),
            }),
            retry: (name, imageDigest) => done('extension_retry', {
                name,
                image_digest: immutableDigest(imageDigest, 'extension image'),
            }),
            remove: (name, imageDigest) => done('extension_remove', {
                name,
                image_digest: immutableDigest(imageDigest, 'extension image'),
            }),
            startAcquisition: async (reference) => {
                const started = expect(await session.call('extension_acquisition_start', { reference }), 'extension_acquisition_job');
                exactAcquisitionJob(started.job);
                return started;
            },
            acquisition: async (job) => {
                const exactJob = exactAcquisitionJob(job);
                return exactAcquisitionStatus(exactJob, expect(await session.call('extension_acquisition_status', { job: exactJob }), 'extension_acquisition'));
            },
            cancelAcquisition: (job, revision) => done('extension_acquisition_cancel', { job: exactAcquisitionJob(job), revision }),
            install: async (job, revision, imageDigest, granted, containers = { selectors: [], create: false }, images = { read: [], use: [], pull: [], remove: [], prune_all_unused: false }, networks = { selectors: [], create: false }, volumes = { selectors: [], create: false }, filesystem = { read: [], write: [], create: [], delete: [], rename: [] }, workspaceEnvironment = { read: [], write: [] }) => expect(await session.call('extension_install', {
                job,
                revision,
                image_digest: immutableDigest(imageDigest, 'extension candidate image'),
                granted,
                containers,
                images,
                networks,
                volumes,
                filesystem,
                workspace_environment: workspaceEnvironment,
            }), 'extension'),
            update: async (job, revision, imageDigest, granted, containers = { selectors: [], create: false }, images = { read: [], use: [], pull: [], remove: [], prune_all_unused: false }, networks = { selectors: [], create: false }, volumes = { selectors: [], create: false }, filesystem = { read: [], write: [], create: [], delete: [], rename: [] }, workspaceEnvironment = { read: [], write: [] }) => expect(await session.call('extension_update', {
                job,
                revision,
                image_digest: immutableDigest(imageDigest, 'extension candidate image'),
                granted,
                containers,
                images,
                networks,
                volumes,
                filesystem,
                workspace_environment: workspaceEnvironment,
            }), 'extension'),
        },
        containers: {
            list: async () => {
                const containers = expect(await session.call('container_list'), 'containers');
                if (new Set(containers.map(({ id }) => id)).size !== containers.length) {
                    throw new TypeError('host returned duplicate immutable container identities; no container selection was assumed');
                }
                return containers;
            },
            inspect: async (id) => {
                const exactId = immutableIdentity(id, [32, 64], 'container');
                const container = expect(await session.call('container_inspect', { id: exactId }), 'container');
                const matches = container.id === exactId ||
                    (exactId.length === 32 && container.id.length === 64 && container.id.startsWith(exactId));
                if (!matches) {
                    throw new TypeError(`host returned container ${container.id}, expected ${exactId}; no container state was assumed`);
                }
                return container;
            },
            inspectObserved: async (id, generation) => {
                const observedGeneration = containerMutation(id, generation).generation;
                const container = expect(await session.call('container_inspect_observed', {
                    id: immutableIdentity(id, [32, 64], 'container'),
                    generation: observedGeneration,
                }), 'container');
                if (container.id !== id || container.generation !== observedGeneration)
                    throw new TypeError('host returned a different observed container generation');
                return container;
            },
            processes: async (id, { snapshot, after = 0, limit = 128, } = {}) => {
                if (!Number.isSafeInteger(after) || after < 0)
                    throw new RangeError('container process cursor must be a nonnegative safe integer');
                if (!Number.isSafeInteger(limit) || limit < 1 || limit > 128)
                    throw new RangeError('container process page limit must be between 1 and 128');
                if (snapshot !== undefined && !/^[0-9a-fA-F]{64}$/.test(snapshot))
                    throw new TypeError('container process snapshot must be 64 hexadecimal characters');
                if (after > 0 && snapshot === undefined)
                    throw new TypeError('container process continuation requires its snapshot identity');
                const page = expect(await session.call('container_processes', { id, snapshot, after, limit }), 'processes');
                if (/^(?:[0-9a-fA-F]{32}|[0-9a-fA-F]{64})$/.test(id) && page.container_id !== id) {
                    throw new TypeError(`host returned processes for container ${page.container_id}, expected ${id}; no process snapshot was assumed`);
                }
                return page;
            },
            processPages: async function* (id, { limit = 128, signal } = {}) {
                let snapshot;
                let after = 0;
                for (;;) {
                    requireOutputActive(signal);
                    const page = await api.containers.processes(id, { snapshot, after, limit });
                    snapshot ??= page.snapshot;
                    if (page.snapshot !== snapshot)
                        throw new Error('host mixed container process snapshots');
                    const more = page.more;
                    const next = page.next;
                    yield page;
                    if (!more)
                        return;
                    if (next === null || next <= after)
                        throw new Error('host returned an invalid container process continuation');
                    after = next;
                }
            },
            logs: async (id, { stdout = true, stderr = true } = {}) => expect(await session.call('container_logs', { id, stdout, stderr }), 'logs'),
            execution: async (id) => {
                const executionId = immutableIdentity(id, [32], 'execution');
                return exactExecution(expect(await session.call('execution_inspect', { id: executionId }), 'execution'), executionId, 'inspection');
            },
            executions: async () => {
                const inventory = expect(await session.call('execution_list'), 'executions');
                if (new Set(inventory.executions.map(({ id }) => id)).size !== inventory.executions.length) {
                    throw new TypeError('host returned duplicate immutable execution identities; no execution selection was assumed');
                }
                return inventory;
            },
            executionLogs: async (id, { stdout = true, stderr = true } = {}) => expect(await session.call('execution_logs', {
                id: immutableIdentity(id, [32], 'execution'),
                stdout,
                stderr,
            }), 'logs'),
            executionOutput: async (id, { after = 0, limit = 16 } = {}) => {
                if (!Number.isSafeInteger(after) || after < 0)
                    throw new RangeError('execution output cursor must be a nonnegative safe integer');
                exactExecutionPageLimit(limit);
                const executionId = immutableIdentity(id, [32], 'execution');
                return exactExecutionOutputPage(expect(await session.call('execution_output', {
                    id: executionId,
                    after,
                    limit,
                }), 'execution_output'), limit, executionId, after);
            },
            executionOutputPages: async function* (id, { after = 0, limit = 16, pollIntervalMs = 50, signal, } = {}) {
                exactExecutionPollInterval(pollIntervalMs);
                const executionId = immutableIdentity(id, [32], 'execution');
                let cursor = after;
                for (;;) {
                    requireOutputActive(signal);
                    const page = await api.containers.executionOutput(executionId, { after: cursor, limit });
                    requireOutputActive(signal);
                    if (page.gap)
                        throw new ExecutionOutputGapError(executionId, cursor, page.next);
                    const next = page.next;
                    const eof = page.eof;
                    const more = page.more;
                    yield page;
                    cursor = next;
                    if (eof)
                        return;
                    if (!more)
                        await outputPoll(pollIntervalMs, signal);
                }
            },
            resumeExecutionStreaming: async (id, { after = 0, pageLimit = 16, maxPages = 4_096, pollIntervalMs = 25, signal, } = {}, onPage) => {
                if (typeof onPage !== 'function')
                    throw new TypeError('resumed streaming execution requires an output callback');
                if (!Number.isSafeInteger(after) || after < 0)
                    throw new RangeError('execution output cursor must be a nonnegative safe integer');
                exactExecutionPageLimit(pageLimit);
                if (!Number.isSafeInteger(maxPages) || maxPages < 1 || maxPages > 1_000_000)
                    throw new RangeError('resumed execution maxPages must be an integer between 1 and 1000000');
                exactExecutionPollInterval(pollIntervalMs);
                requireOutputActive(signal);
                const executionId = immutableIdentity(id, [32], 'execution');
                let cursor = after;
                let pages = 0;
                let phase = 'output';
                try {
                    for await (const page of api.containers.executionOutputPages(executionId, {
                        after: cursor,
                        limit: pageLimit,
                        pollIntervalMs,
                        signal,
                    })) {
                        await outputStep(() => onPage(page), signal);
                        cursor = page.next;
                        pages += 1;
                        if (!page.eof && pages === maxPages) {
                            return { executionId, next: cursor, pages, complete: false };
                        }
                    }
                    phase = 'inspect';
                    requireOutputActive(signal);
                    const execution = await api.containers.execution(executionId);
                    return { executionId, execution, next: cursor, pages, complete: true };
                }
                catch (cause) {
                    throw new ExecutionOperationError(executionId, phase, cause, undefined, cursor);
                }
            },
            waitExecution: async (id, { timeoutMs = 30_000 } = {}) => {
                const executionId = immutableIdentity(id, [32], 'execution');
                return exactExecution(expect(await session.call('execution_wait', {
                    id: executionId,
                    timeout_ms: timeoutMs,
                }), 'execution'), executionId, 'wait result');
            },
            signalExecution: (id, signal) => done('execution_kill', {
                id: immutableIdentity(id, [32], 'execution'),
                signal: exactExecutionSignal(signal),
            }),
            cancelExecution: (id, { signal = 'SIGTERM', timeoutMs = 1_000 } = {}) => {
                return done('execution_cancel', {
                    id: immutableIdentity(id, [32], 'execution'),
                    signal: exactExecutionSignal(signal),
                    timeout_ms: exactExecutionCancellation(timeoutMs),
                });
            },
            removeExecution: (id) => done('execution_remove', {
                id: immutableIdentity(id, [32], 'execution'),
            }),
            writeExecutionStdin: (id, input) => {
                const contents = exactExecutionInput(input);
                return done('execution_write', {
                    id: immutableIdentity(id, [32], 'execution'),
                    contents: [...contents],
                });
            },
            closeExecutionStdin: (id) => done('execution_close_input', {
                id: immutableIdentity(id, [32], 'execution'),
            }),
            pipeExecutionStdin: async (id, source, { signal, close = true } = {}) => {
                const executionId = immutableIdentity(id, [32], 'execution');
                if (source === null ||
                    source === undefined ||
                    (typeof source[Symbol.asyncIterator] !== 'function' &&
                        typeof source[Symbol.iterator] !== 'function')) {
                    throw new TypeError('execution stdin source must be an iterable of bounded chunks');
                }
                let chunks = 0;
                let bytes = 0;
                const iterator = typeof source[Symbol.asyncIterator] === 'function'
                    ? source[Symbol.asyncIterator]()
                    : source[Symbol.iterator]();
                for (;;) {
                    requireOutputActive(signal);
                    const item = await outputStep(() => iterator.next(), signal);
                    if (item.done)
                        break;
                    const chunk = item.value;
                    const contents = exactExecutionInput(chunk);
                    await api.containers.writeExecutionStdin(executionId, contents);
                    chunks += 1;
                    bytes += contents.byteLength;
                    requireOutputActive(signal);
                }
                if (close) {
                    requireOutputActive(signal);
                    await api.containers.closeExecutionStdin(executionId);
                }
                return { chunks, bytes, closed: close };
            },
            create: async (configuration) => {
                if (!configuration || typeof configuration !== 'object' || Array.isArray(configuration)) {
                    throw new TypeError('container creation requires a configuration object');
                }
                const spec = {
                    hostname: null,
                    entrypoint: null,
                    command: [],
                    environment: [],
                    working_directory: null,
                    user: null,
                    labels: [],
                    mounts: [],
                    network: null,
                    ports: [],
                    memory_mb: null,
                    cpus: null,
                    pids_limit: null,
                    ...configuration,
                };
                const normalized = {
                    ...spec,
                    mounts: spec.mounts.map((mount) => ({ read_only: false, ...mount })),
                };
                return expect(await session.call('container_create', { spec: normalized }), 'identity');
            },
            start: (id, generation) => done('container_start', containerMutation(id, generation)),
            stop: (id, generation) => done('container_stop', containerMutation(id, generation)),
            remove: (id, generation) => done('container_remove', containerMutation(id, generation)),
            pause: (id, generation) => done('container_pause', containerMutation(id, generation)),
            unpause: (id, generation) => done('container_unpause', containerMutation(id, generation)),
            restart: (id, generation) => done('container_restart', containerMutation(id, generation)),
            rename: (id, generation, name) => done('container_rename', {
                ...containerMutation(id, generation),
                name: exactContainerName(name),
            }),
            kill: (id, generation, signal) => done('container_kill', { ...containerMutation(id, generation), signal }),
            exec: async (id, generation, { command, environment = [], user, workingDirectory, stdin = false, } = {}) => {
                requireCapabilities('containers:execute', ...(stdin ? ['containers:input'] : []));
                return expect(await session.call('container_exec', {
                    ...containerMutation(id, generation),
                    command,
                    environment: exactExecEnvironment(environment),
                    user: user ?? null,
                    working_directory: workingDirectory ?? null,
                    ...(stdin ? { stdin: true } : {}),
                }), 'identity');
            },
            execWithCredentials: async (id, generation, { command, environment = [], credentials, user, workingDirectory, stdin = false }) => {
                const exactEnvironment = exactExecEnvironment(environment);
                requireCapabilities('containers:execute', 'credentials:inject', ...(stdin ? ['containers:input'] : []));
                return expect(await session.call('container_exec_credential', {
                    ...containerMutation(id, generation),
                    command,
                    environment: exactEnvironment,
                    credentials: exactExecCredentials(credentials ?? [], exactEnvironment),
                    user: user ?? null,
                    working_directory: workingDirectory ?? null,
                    ...(stdin ? { stdin: true } : {}),
                }), 'identity');
            },
            execAndWait: async (id, generation, { command, environment = [], user, workingDirectory, ...waitOptions } = {}) => {
                const containerId = immutableIdentity(id, [32, 64], 'container');
                const argv = exactCommand(command);
                const { timeoutMs, stdout, stderr } = exactExecutionWaitOptions(waitOptions);
                const executionId = await api.containers.exec(containerId, generation, {
                    command: argv,
                    environment,
                    user,
                    workingDirectory,
                });
                let phase = 'wait';
                let execution;
                try {
                    execution = await api.containers.waitExecution(executionId, { timeoutMs });
                    phase = 'logs';
                    const output = await api.containers.executionLogs(executionId, { stdout, stderr });
                    return { execution, output };
                }
                catch (cause) {
                    throw new ExecutionOperationError(executionId, phase, cause, execution);
                }
            },
            execStreaming: async (id, generation, { command, environment = [], credentials, user, workingDirectory, input, pageLimit = 16, pollIntervalMs = 25, signal, deadlineMs, cancelSignal = 'SIGTERM', cancelTimeoutMs = 1_000, onStarted, }, onPage) => {
                if (typeof onPage !== 'function')
                    throw new TypeError('streaming execution requires an output callback');
                if (onStarted !== undefined && typeof onStarted !== 'function')
                    throw new TypeError('streaming execution onStarted must be a function');
                exactExecutionPageLimit(pageLimit);
                exactExecutionPollInterval(pollIntervalMs);
                exactExecutionDeadline(deadlineMs);
                exactExecutionSignal(cancelSignal);
                exactExecutionCancellation(cancelTimeoutMs);
                requireOutputActive(signal);
                const executionId = credentials?.length
                    ? await api.containers.execWithCredentials(id, generation, {
                        command,
                        environment,
                        credentials,
                        user,
                        workingDirectory,
                        stdin: input !== undefined,
                    })
                    : await api.containers.exec(id, generation, {
                        command,
                        environment,
                        user,
                        workingDirectory,
                        stdin: input !== undefined,
                    });
                let phase = 'output';
                const streaming = new AbortController();
                const inputStreaming = new AbortController();
                const deadline = deadlineMs === undefined
                    ? undefined
                    : setTimeout(() => {
                        const reason = new ExecutionDeadlineError(executionId, deadlineMs);
                        streaming.abort(reason);
                        inputStreaming.abort(reason);
                    }, deadlineMs);
                deadline?.unref?.();
                const stopStreaming = () => {
                    streaming.abort(signal?.reason);
                    inputStreaming.abort(signal?.reason);
                };
                if (signal?.aborted)
                    stopStreaming();
                else
                    signal?.addEventListener('abort', stopStreaming, { once: true });
                try {
                    requireOutputActive(streaming.signal);
                    if (onStarted)
                        await outputStep(() => onStarted(executionId), streaming.signal);
                    const consumeOutput = async () => {
                        let complete = false;
                        try {
                            for await (const page of api.containers.executionOutputPages(executionId, {
                                limit: pageLimit,
                                pollIntervalMs,
                                signal: streaming.signal,
                            })) {
                                await outputStep(() => onPage(page), streaming.signal);
                            }
                            complete = true;
                        }
                        catch (cause) {
                            throw { phase: 'output', cause };
                        }
                        finally {
                            if (complete)
                                inputStreaming.abort('execution output reached EOF');
                        }
                    };
                    const sendInput = async () => {
                        if (input === undefined)
                            return;
                        try {
                            await api.containers.pipeExecutionStdin(executionId, input, {
                                signal: inputStreaming.signal,
                                close: true,
                            });
                        }
                        catch (cause) {
                            if (inputStreaming.signal.aborted &&
                                !streaming.signal.aborted &&
                                cause instanceof Error &&
                                cause.name === 'AbortError')
                                return;
                            throw { phase: 'input', cause };
                        }
                    };
                    try {
                        await Promise.all([consumeOutput(), sendInput()]);
                    }
                    catch (failure) {
                        streaming.abort();
                        inputStreaming.abort();
                        if (failure !== null &&
                            typeof failure === 'object' &&
                            'phase' in failure &&
                            'cause' in failure) {
                            phase = String(failure.phase);
                            throw failure.cause;
                        }
                        throw failure;
                    }
                    phase = 'inspect';
                    const execution = await api.containers.execution(executionId);
                    if (execution.running)
                        throw new ExecutionOutputEndedEarlyError(executionId);
                    return { executionId, execution };
                }
                catch (cause) {
                    await api.containers
                        .cancelExecution(executionId, { signal: cancelSignal, timeoutMs: cancelTimeoutMs })
                        .catch(() => { });
                    throw new ExecutionOperationError(executionId, phase, cause);
                }
                finally {
                    if (deadline !== undefined)
                        clearTimeout(deadline);
                    signal?.removeEventListener('abort', stopStreaming);
                }
            },
            execText: async (id, generation, configuration) => {
                const { maxBytes, ...options } = configuration;
                if (!Number.isSafeInteger(maxBytes) || maxBytes < 1 || maxBytes > 16 * 1024 * 1024) {
                    throw new RangeError('execution text maxBytes must be between 1 and 16777216');
                }
                let bytes = 0;
                let stdout = '';
                let stderr = '';
                // Query/result protocols are textual. Replacing malformed bytes with U+FFFD could silently
                // change a database value or delimiter, so fail and cancel the owned execution instead.
                const stdoutDecoder = new TextDecoder('utf-8', { fatal: true });
                const stderrDecoder = new TextDecoder('utf-8', { fatal: true });
                const result = await api.containers.execStreaming(id, generation, options, (page) => {
                    for (const entry of page.entries) {
                        bytes += entry.bytes.length;
                        if (bytes > maxBytes) {
                            throw new RangeError(`execution text exceeded the ${maxBytes} byte limit`);
                        }
                        const decoder = entry.stream === 'stdout' ? stdoutDecoder : stderrDecoder;
                        const text = decoder.decode(Uint8Array.from(entry.bytes), { stream: true });
                        if (entry.stream === 'stdout')
                            stdout += text;
                        else
                            stderr += text;
                    }
                });
                try {
                    stdout += stdoutDecoder.decode();
                    stderr += stderrDecoder.decode();
                }
                catch (cause) {
                    throw new ExecutionOperationError(result.executionId, 'output', cause, result.execution);
                }
                return { ...result, stdout, stderr };
            },
            execLines: async (id, generation, configuration, onLine) => {
                const { maxLineBytes, maxLines, onStderr, ...options } = configuration;
                if (!Number.isSafeInteger(maxLineBytes) ||
                    maxLineBytes < 1 ||
                    maxLineBytes > 16 * 1024 * 1024) {
                    throw new RangeError('execution line maxLineBytes must be between 1 and 16777216');
                }
                if (typeof onLine !== 'function')
                    throw new TypeError('lines execution requires a line callback');
                if (onStderr !== undefined && typeof onStderr !== 'function')
                    throw new TypeError('lines execution onStderr must be a function');
                exactExecutionLineLimit(maxLines);
                let pending = [];
                let lines = 0;
                const decoder = new TextDecoder('utf-8', { fatal: true });
                const stderrDecoder = new TextDecoder('utf-8', { fatal: true });
                const deliver = async () => {
                    let bytes = pending;
                    pending = [];
                    if (bytes.at(-1) === 13)
                        bytes = bytes.slice(0, -1);
                    if (bytes.length > maxLineBytes)
                        throw new RangeError(`execution line exceeded the ${maxLineBytes} byte limit`);
                    const text = decoder.decode(Uint8Array.from(bytes));
                    const line = lines + 1;
                    if (maxLines !== undefined && line > maxLines) {
                        throw new RangeError(`execution output exceeded the ${maxLines} line limit`);
                    }
                    await onLine(text, line);
                    lines = line;
                };
                const result = await api.containers.execStreaming(id, generation, options, async (page) => {
                    for (const entry of page.entries) {
                        if (entry.stream === 'stderr') {
                            const text = stderrDecoder.decode(Uint8Array.from(entry.bytes), { stream: true });
                            if (text && onStderr)
                                await onStderr(text);
                            continue;
                        }
                        for (const byte of entry.bytes) {
                            if (byte === 10)
                                await deliver();
                            else {
                                pending.push(byte);
                                if (pending.length > maxLineBytes)
                                    throw new RangeError(`execution line exceeded the ${maxLineBytes} byte limit`);
                            }
                        }
                    }
                });
                try {
                    if (pending.length > 0)
                        await outputStep(deliver, options.signal);
                    const stderrTail = stderrDecoder.decode();
                    if (stderrTail && onStderr) {
                        await outputStep(() => onStderr(stderrTail), options.signal);
                    }
                }
                catch (cause) {
                    throw new ExecutionOperationError(result.executionId, 'output', cause, result.execution);
                }
                return { ...result, lines };
            },
            execJsonLines: async (id, generation, configuration, onValue) => {
                if (typeof onValue !== 'function')
                    throw new TypeError('JSON lines execution requires a value callback');
                const { decode = (value) => value, ...options } = configuration;
                if (typeof decode !== 'function')
                    throw new TypeError('JSON lines execution decode must be a function');
                return api.containers.execLines(id, generation, options, async (text, line) => {
                    let value;
                    try {
                        value = JSON.parse(text);
                    }
                    catch (cause) {
                        throw new JsonLineParseError(line, cause);
                    }
                    let decoded;
                    try {
                        decoded = decode(value, line);
                    }
                    catch (cause) {
                        throw new JsonLineDecodeError(line, cause);
                    }
                    await onValue(decoded, line);
                });
            },
            attachTerminal: (id, command) => session
                .call('container_attach_terminal', {
                id: immutableIdentity(id, [32, 64], 'container'),
                command: exactCommand(command),
            })
                .then((reply) => expect(reply, 'identity')),
        },
        images: {
            inventory: async () => {
                const inventory = expect(await session.call('image_list'), 'images');
                exactImages(inventory.images);
                return inventory;
            },
            list: async () => (await api.images.inventory()).images,
            inspect: async (reference) => {
                const image = expect(await session.call('image_inspect', { reference }), 'image_details');
                if (/^sha256:[0-9a-f]{64}$/.test(reference) && image.id !== reference) {
                    throw new TypeError(`host returned image ${image.id}, expected ${reference}; no image state was assumed`);
                }
                return image;
            },
            pull: async (reference, { timeoutMs = 120_000, signal } = {}) => {
                if (!Number.isSafeInteger(timeoutMs) || timeoutMs < 1 || timeoutMs > 86_400_000) {
                    throw new RangeError('image pull timeoutMs must be an integer from 1 to 86400000');
                }
                requireOutputActive(signal);
                const { job } = expect(await session.call('image_pull_start', { reference }), 'image_pull_job');
                const deadline = Date.now() + timeoutMs;
                try {
                    for (;;) {
                        requireOutputActive(signal);
                        if (Date.now() >= deadline)
                            throw new Error(`image pull timed out after ${timeoutMs}ms`);
                        const status = expect(await session.call('image_pull_status', { job }), 'image_pull');
                        if (status.state === 'complete' && status.image)
                            return status.image;
                        if (status.state === 'failed')
                            throw new Error(status.error ?? 'image pull failed');
                        if (status.state === 'cancelled')
                            throw new Error('image pull was cancelled');
                        await outputPoll(Math.min(100, Math.max(1, deadline - Date.now())), signal);
                    }
                }
                catch (error) {
                    try {
                        await done('image_pull_cancel', { job });
                    }
                    catch {
                        /* Preserve the primary error. */
                    }
                    throw error;
                }
            },
            startPull: async (reference) => expect(await session.call('image_pull_start', { reference }), 'image_pull_job'),
            pullStatus: async (job) => expect(await session.call('image_pull_status', { job }), 'image_pull'),
            cancelPull: (job) => done('image_pull_cancel', { job }),
            remove: (reference) => done('image_remove', { reference: immutableDigest(reference, 'image') }),
            prune: async () => expect(await session.call('image_prune'), 'image_prune'),
        },
        volumes: {
            inventory: async () => {
                const inventory = expect(await session.call('volume_list'), 'volumes');
                exactVolumes(inventory.volumes);
                return inventory;
            },
            list: async () => (await api.volumes.inventory()).volumes,
            inspect: async (name) => {
                const volume = expect(await session.call('volume_inspect', { name }), 'volume');
                if (volume.name !== name) {
                    throw new TypeError(`host returned volume ${volume.name}, expected ${name}; no volume state was assumed`);
                }
                return volume;
            },
            create: async (name) => expect(await session.call('volume_create', { name }), 'volume'),
            remove: (name, generation) => done('volume_remove', {
                name,
                generation: immutableIdentity(generation, [32], 'volume generation'),
            }),
        },
        networks: {
            inventory: async () => {
                const inventory = expect(await session.call('network_list'), 'networks');
                exactNetworks(inventory.networks);
                return inventory;
            },
            list: async () => (await api.networks.inventory()).networks,
            inspect: async (reference) => {
                const network = expect(await session.call('network_inspect', { reference }), 'network');
                if (/^[0-9a-fA-F]{32}$/.test(reference) && network.id !== reference) {
                    throw new TypeError(`host returned network ${network.id}, expected ${reference}; no network state was assumed`);
                }
                return network;
            },
            create: async (name) => expect(await session.call('network_create', { name }), 'identity'),
            remove: (reference) => done('network_remove', { reference: immutableIdentity(reference, [32], 'network') }),
            connect: (reference, container, options) => {
                const aliases = endpointAliases(options);
                const withValue = {
                    reference: immutableIdentity(reference, [32], 'network'),
                    container: immutableIdentity(container, [32, 64], 'container'),
                };
                if (aliases.length > 0)
                    withValue.aliases = aliases;
                return done('network_connect', withValue);
            },
            disconnect: (reference, container) => done('network_disconnect', {
                reference: immutableIdentity(reference, [32], 'network'),
                container: immutableIdentity(container, [32, 64], 'container'),
            }),
        },
        terminal: {
            panes: async () => exactPaneInventory(expect(await session.call('pane_list'), 'panes')),
            tabs: async () => exactTabs(expect(await session.call('terminal_tabs'), 'tabs')),
            topology: async () => exactTopology(expect(await session.call('terminal_topology'), 'topology')),
            openTab: async (title) => expect(await session.call('terminal_open_tab', { title }), 'identity'),
            split: async (slot, division) => expect(await session.call('terminal_split', { slot, division }), 'identity'),
            splitObserved: (slot, generation, revision, division) => {
                if (!Number.isSafeInteger(generation) ||
                    generation < 0 ||
                    !Number.isSafeInteger(revision) ||
                    revision < 0) {
                    throw new TypeError('terminal split requires nonnegative safe integer generation and revision');
                }
                return session
                    .call('terminal_split_observed', { slot, generation, revision, division })
                    .then((reply) => expect(reply, 'identity'));
            },
            spawn: (slot, command) => {
                if (!Array.isArray(command) ||
                    command.length === 0 ||
                    command.length > 64 ||
                    command.some((argument) => typeof argument !== 'string' ||
                        new TextEncoder().encode(argument).byteLength > 4096 ||
                        argument.includes('\0')) ||
                    command[0].length === 0 ||
                    command.reduce((bytes, argument) => bytes + new TextEncoder().encode(argument).byteLength, 0) >
                        32 * 1024) {
                    throw new RangeError('terminal command must contain 1..=64 NUL-free arguments, each at most 4096 bytes and 32768 bytes in aggregate');
                }
                return done('terminal_spawn', { slot, command: [...command] });
            },
            spawnObserved: (slot, generation, revision, command) => {
                if (!Number.isSafeInteger(generation) ||
                    generation < 0 ||
                    !Number.isSafeInteger(revision) ||
                    revision < 0) {
                    throw new TypeError('terminal spawn requires nonnegative safe integer generation and revision');
                }
                if (!Array.isArray(command) ||
                    command.length === 0 ||
                    command.length > 64 ||
                    command.some((argument) => typeof argument !== 'string' ||
                        new TextEncoder().encode(argument).byteLength > 4096 ||
                        argument.includes('\0')) ||
                    command[0].length === 0 ||
                    command.reduce((bytes, argument) => bytes + new TextEncoder().encode(argument).byteLength, 0) >
                        32 * 1024) {
                    throw new RangeError('terminal command must contain 1..=64 NUL-free arguments, each at most 4096 bytes and 32768 bytes in aggregate');
                }
                return done('terminal_spawn_observed', {
                    slot,
                    generation,
                    revision,
                    command: [...command],
                });
            },
            read: async (slot, lines) => exactPane(expect(await session.call('terminal_read_pane', {
                slot,
                lines: exactTerminalReadLines(lines),
            }), 'text'), slot, 'terminal text'),
            semantics: async (slot) => exactPane(expect(await session.call('pane_semantic_read', { slot }), 'semantics'), slot, 'pane semantics'),
            /** Converts either a terminal or a native UI pane into bounded agent-readable text. */
            toText: async (slot, { lines } = {}) => {
                lines = exactTerminalReadLines(lines);
                const inventory = exactPaneInventory(expect(await session.call('pane_list'), 'panes'));
                const pane = inventory.panes.find((candidate) => candidate.slot === slot);
                if (!pane) {
                    throw new PaneUnavailableError(slot, inventory.truncated ? 'inventory-truncated' : 'absent');
                }
                if (pane.kind === 'terminal') {
                    const snapshot = exactPane(expect(await session.call('terminal_read_pane', { slot, lines }), 'text'), slot, 'terminal text');
                    if (snapshot.generation !== pane.generation || snapshot.revision !== pane.revision) {
                        throw new PaneChangedError(slot, pane, snapshot);
                    }
                    return { kind: 'terminal', text: snapshot.lines.join('\n'), snapshot };
                }
                const snapshot = exactPane(expect(await session.call('pane_semantic_read', { slot }), 'semantics'), slot, 'pane semantics');
                if (snapshot.generation !== pane.generation || snapshot.revision !== pane.revision) {
                    throw new PaneChangedError(slot, pane, snapshot);
                }
                return { kind: 'ui', snapshot, ...semanticText(snapshot) };
            },
            readAll: async ({ lines } = {}) => {
                lines = exactTerminalReadLines(lines);
                const inventory = exactPaneInventory(expect(await session.call('pane_list'), 'panes'));
                const panes = [];
                for (const pane of inventory.panes) {
                    if (pane.kind === 'terminal') {
                        const snapshot = expect(await session.call('terminal_read_pane', { slot: pane.slot, lines }), 'text');
                        if (snapshot.slot !== pane.slot ||
                            snapshot.generation !== pane.generation ||
                            snapshot.revision !== pane.revision) {
                            throw new PaneChangedError(pane.slot, pane, snapshot);
                        }
                        panes.push({
                            pane,
                            readable: { kind: 'terminal', text: snapshot.lines.join('\n'), snapshot },
                        });
                    }
                    else {
                        const snapshot = expect(await session.call('pane_semantic_read', { slot: pane.slot }), 'semantics');
                        if (snapshot.slot !== pane.slot ||
                            snapshot.generation !== pane.generation ||
                            snapshot.revision !== pane.revision) {
                            throw new PaneChangedError(pane.slot, pane, snapshot);
                        }
                        panes.push({
                            pane,
                            readable: { kind: 'ui', snapshot, ...semanticText(snapshot) },
                        });
                    }
                }
                return { panes, complete: !inventory.truncated };
            },
            act: (slot, action) => {
                return done('pane_semantic_action', { slot, action: exactSemanticAction(action) });
            },
            writeInput: (slot, generation, revision, input) => {
                if (!Number.isSafeInteger(generation) ||
                    generation < 0 ||
                    !Number.isSafeInteger(revision) ||
                    revision < 0) {
                    throw new TypeError('terminal input requires nonnegative safe integer generation and revision');
                }
                const contents = exactPaneInput(input);
                return done('terminal_write_pane', { slot, generation, revision, contents: [...contents] });
            },
            resizeGrid: (slot, columns, rows) => {
                if (!Number.isInteger(columns) ||
                    !Number.isInteger(rows) ||
                    columns < 1 ||
                    rows < 1 ||
                    columns > 1000 ||
                    rows > 1000) {
                    throw new RangeError('terminal grid rows and columns must be integers within 1..=1000');
                }
                return done('terminal_resize_grid', { slot, columns, rows });
            },
            resizeGridObserved: (slot, generation, revision, columns, rows) => {
                if (!Number.isSafeInteger(generation) ||
                    generation < 0 ||
                    !Number.isSafeInteger(revision) ||
                    revision < 0)
                    throw new TypeError('terminal resize requires nonnegative safe integer generation and revision');
                if (!Number.isInteger(columns) ||
                    !Number.isInteger(rows) ||
                    columns < 1 ||
                    rows < 1 ||
                    columns > 1000 ||
                    rows > 1000)
                    throw new RangeError('terminal grid rows and columns must be integers within 1..=1000');
                return done('terminal_resize_grid_observed', { slot, generation, revision, columns, rows });
            },
            close: (slot) => done('terminal_close_pane', { slot }),
            pinTab: (tab, pinned = true) => done('terminal_pin_tab', { tab, pinned: Boolean(pinned) }),
            closeObserved: (slot, generation, revision) => {
                if (!Number.isSafeInteger(generation) ||
                    generation < 0 ||
                    !Number.isSafeInteger(revision) ||
                    revision < 0) {
                    throw new TypeError('terminal close requires nonnegative safe integer generation and revision');
                }
                return done('terminal_close_pane_observed', { slot, generation, revision });
            },
            focus: (slot) => done('terminal_focus_pane', { slot }),
            focusObserved: (slot, generation, revision) => {
                if (!Number.isSafeInteger(generation) ||
                    generation < 0 ||
                    !Number.isSafeInteger(revision) ||
                    revision < 0)
                    throw new TypeError('terminal focus requires nonnegative safe integer generation and revision');
                return done('terminal_focus_pane_observed', { slot, generation, revision });
            },
            retitle: (slot, title) => done('terminal_retitle_pane', { slot, title: exactPaneTitle(title) }),
            retitleObserved: (slot, generation, revision, title) => {
                if (!Number.isSafeInteger(generation) ||
                    generation < 0 ||
                    !Number.isSafeInteger(revision) ||
                    revision < 0)
                    throw new TypeError('terminal retitle requires nonnegative safe integer generation and revision');
                return done('terminal_retitle_pane_observed', {
                    slot,
                    generation,
                    revision,
                    title: exactPaneTitle(title),
                });
            },
            ratio: (slot, ratio) => done('terminal_ratio', { slot, ratio: exactPaneRatio(ratio) }),
            ratioObserved: (slot, generation, revision, ratio) => {
                if (!Number.isSafeInteger(generation) ||
                    generation < 0 ||
                    !Number.isSafeInteger(revision) ||
                    revision < 0) {
                    throw new TypeError('terminal ratio requires nonnegative safe integer generation and revision');
                }
                return done('terminal_ratio_observed', {
                    slot,
                    generation,
                    revision,
                    ratio: exactPaneRatio(ratio),
                });
            },
            switchOccupant: (slot, generation, target) => {
                if (!Number.isSafeInteger(generation) || generation < 0)
                    throw new TypeError('pane generation must be a nonnegative safe integer');
                return done('terminal_switch_occupant', {
                    slot,
                    generation,
                    target: exactOccupantTarget(target),
                });
            },
            switchOccupantObserved: (slot, generation, revision, target) => {
                if (!Number.isSafeInteger(generation) ||
                    generation < 0 ||
                    !Number.isSafeInteger(revision) ||
                    revision < 0)
                    throw new TypeError('pane occupant switch requires nonnegative safe integer generation and revision');
                return done('terminal_switch_occupant_observed', {
                    slot,
                    generation,
                    revision,
                    target: exactOccupantTarget(target),
                });
            },
        },
        files: {
            pathGrant: (operation, path) => {
                if (!['read', 'write', 'create', 'delete', 'rename'].includes(operation))
                    throw new TypeError('filesystem grant operation must be read, write, create, delete, or rename');
                encodeRequest('filesystem_stat', { path });
                const selectors = session.grantedFilesystem[operation];
                if (selectors.some((selector) => 'exact' in selector && selector.exact === path))
                    return 'exact';
                return selectors.some((selector) => 'subtree' in selector && filesystemSelectorPermits(selector, path))
                    ? 'subtree'
                    : null;
            },
            inventory: async () => {
                const inventory = expect(await session.call('filesystem_inventory'), 'file_inventory');
                exactFilesystemJournal(inventory.journal);
                return inventory;
            },
            beginWalk: async (path, options = {}) => {
                requireFilesystemActive(options.signal);
                const scoped = options.signal ? api.withSignal(options.signal) : api;
                const inventory = await scoped.files.inventory();
                requireFilesystemActive(options.signal);
                return {
                    inventory,
                    cursor: { journal: inventory.journal, revision: inventory.revision },
                    entries: scoped.files.walk(path, options),
                };
            },
            changes: async ({ journal, revision: after }, limit = 256) => {
                exactFilesystemJournal(journal);
                if (!Number.isSafeInteger(after) || after < 0)
                    throw new TypeError('filesystem change cursor must be a nonnegative safe integer');
                exactFilesystemPageSize(limit);
                const page = expect(await session.call('filesystem_changes', { observed: journal, after, limit }), 'file_changes');
                exactFilesystemJournal(page.journal);
                if ((page.journal !== journal && !page.truncated) ||
                    page.changes.length > limit ||
                    (!page.truncated && page.next < after) ||
                    page.current < page.next ||
                    page.changes.some((change, index) => change.revision <= after ||
                        change.revision > page.next ||
                        (index > 0 && change.revision <= page.changes[index - 1].revision) ||
                        change.path !== (change.entry?.path ?? change.path)) ||
                    (page.changes.length > 0 && page.next < page.changes.at(-1).revision) ||
                    (page.truncated && (page.changes.length > 0 || page.next !== page.current)) ||
                    (page.more && page.next >= page.current))
                    throw new TypeError('host returned an inconsistent filesystem change page');
                return page;
            },
            catchUpChanges: async ({ cursor, pageSize = 256, maxChanges = 4_096, maxPages = 64, signal, }) => {
                exactFilesystemJournal(cursor?.journal);
                if (!Number.isSafeInteger(cursor?.revision) || cursor.revision < 0)
                    throw new TypeError('filesystem change cursor must be a nonnegative safe integer');
                exactFilesystemPageSize(pageSize);
                if (!Number.isSafeInteger(maxChanges) || maxChanges < 1 || maxChanges > 65_536)
                    throw new RangeError('filesystem catch-up maxChanges must be an integer between 1 and 65536');
                if (!Number.isSafeInteger(maxPages) || maxPages < 1 || maxPages > 256)
                    throw new RangeError('filesystem catch-up maxPages must be an integer between 1 and 256');
                requireFilesystemActive(signal);
                const scoped = signal ? api.withSignal(signal) : api;
                let journal = cursor.journal;
                let revision = cursor.revision;
                let current = revision;
                const changes = [];
                for (let pageIndex = 0; pageIndex < maxPages; pageIndex += 1) {
                    requireFilesystemActive(signal);
                    const requested = { journal, revision };
                    const remaining = maxChanges - changes.length;
                    const page = await scoped.files.changes(requested, Math.min(pageSize, remaining));
                    requireFilesystemActive(signal);
                    if (page.truncated) {
                        throw new FilesystemJournalGapError(requested, {
                            journal: page.journal,
                            revision: page.current,
                        });
                    }
                    changes.push(...page.changes);
                    journal = page.journal;
                    revision = page.next;
                    current = page.current;
                    if (!page.more) {
                        return {
                            changes,
                            cursor: { journal, revision },
                            current,
                            caughtUp: true,
                        };
                    }
                    if (changes.length === maxChanges)
                        break;
                }
                return {
                    changes,
                    cursor: { journal, revision },
                    current,
                    caughtUp: false,
                };
            },
            changePages: async function* ({ cursor, pageSize = 256, pollMs = 250, gapPolicy = 'yield', signal, }) {
                let { journal, revision: after } = cursor;
                exactFilesystemPageSize(pageSize);
                if (!Number.isSafeInteger(after) || after < 0)
                    throw new TypeError('filesystem change cursor must be a nonnegative safe integer');
                if (!Number.isSafeInteger(pollMs) || pollMs < 1)
                    throw new TypeError('filesystem change poll interval must be a positive integer');
                if (gapPolicy !== 'yield' && gapPolicy !== 'throw')
                    throw new TypeError("filesystem gapPolicy must be 'yield' or 'throw'");
                const scoped = signal ? api.withSignal(signal) : api;
                for (;;) {
                    requireFilesystemActive(signal);
                    const requestedJournal = journal;
                    const requestedAfter = after;
                    const page = await scoped.files.changes({ journal, revision: after }, pageSize);
                    requireFilesystemActive(signal);
                    if (page.truncated && gapPolicy === 'throw') {
                        throw new FilesystemJournalGapError({ journal: requestedJournal, revision: requestedAfter }, { journal: page.journal, revision: page.current });
                    }
                    journal = page.journal;
                    after = page.next;
                    const more = page.more;
                    if (page.truncated || page.changes.length > 0 || page.next !== requestedAfter)
                        yield page;
                    if (more)
                        continue;
                    await new Promise((resolve, reject) => {
                        const timer = setTimeout(done, pollMs);
                        function done() {
                            signal?.removeEventListener('abort', abort);
                            resolve();
                        }
                        function abort() {
                            clearTimeout(timer);
                            reject(filesystemAbort(signal));
                        }
                        signal?.addEventListener('abort', abort, { once: true });
                        if (signal?.aborted)
                            abort();
                    });
                }
            },
            watchChanges: async (listener, { cursor, pageSize = 256, pollMs = 250, signal, }) => {
                const stopped = new AbortController();
                const abort = () => stopped.abort(signal?.reason);
                if (signal?.aborted)
                    abort();
                else
                    signal?.addEventListener('abort', abort, { once: true });
                const running = (async () => {
                    try {
                        for await (const page of api.files.changePages({
                            cursor,
                            pageSize,
                            pollMs,
                            signal: stopped.signal,
                        })) {
                            await listener(page);
                        }
                    }
                    catch (error) {
                        if (!(stopped.signal.aborted &&
                            error instanceof Error &&
                            error.name === 'AbortError')) {
                            throw error;
                        }
                    }
                    finally {
                        signal?.removeEventListener('abort', abort);
                    }
                })();
                // Mark the background branch handled even when callers supervise it through `done` later.
                void running.catch(() => { });
                const stop = async () => {
                    stopped.abort();
                    await running;
                };
                Object.defineProperty(stop, 'done', { value: running, enumerable: true });
                return stop;
            },
            watchLatestChanges: async (listener, { cursor, pageSize = 256, pollMs = 250, maxBufferedChanges = 1_024, signal, }) => {
                if (typeof listener !== 'function')
                    throw new TypeError('filesystem latest-change listener must be a function');
                if (!Number.isSafeInteger(maxBufferedChanges) ||
                    maxBufferedChanges < 1 ||
                    maxBufferedChanges > 65_536)
                    throw new RangeError('filesystem latest-change buffer must contain between 1 and 65536 changes');
                const stopped = new AbortController();
                const abort = () => stopped.abort(signal?.reason);
                if (signal?.aborted)
                    abort();
                else
                    signal?.addEventListener('abort', abort, { once: true });
                const pending = new Map();
                let buffered = [];
                let current;
                let failure;
                const running = (async () => {
                    try {
                        for await (const page of api.files.changePages({
                            cursor,
                            pageSize,
                            pollMs,
                            signal: stopped.signal,
                        })) {
                            current?.abort('superseded by a newer filesystem revision');
                            if (page.truncated)
                                buffered = [page];
                            else
                                buffered.push(page);
                            const bufferedChanges = buffered.reduce((total, retained) => total + retained.changes.length, 0);
                            if (bufferedChanges > maxBufferedChanges) {
                                throw new RangeError(`filesystem latest-change buffer exceeded ${maxBufferedChanges} changes`);
                            }
                            if (pending.size >= 2) {
                                throw new Error('filesystem latest-change listener retained two superseded generations');
                            }
                            const generation = new AbortController();
                            current = generation;
                            const included = buffered.length;
                            const latest = buffered.at(-1);
                            const changed = new Map(buffered
                                .flatMap((retained) => retained.changes)
                                .map((change) => [change.path, change]));
                            const accumulated = {
                                ...latest,
                                changes: [...changed.values()].sort((left, right) => left.revision - right.revision),
                                truncated: buffered.some((retained) => retained.truncated),
                            };
                            const task = Promise.resolve()
                                .then(() => listener(accumulated, generation.signal))
                                .then(() => {
                                if (!generation.signal.aborted)
                                    buffered.splice(0, included);
                            })
                                .catch((error) => {
                                if (generation.signal.aborted)
                                    return;
                                failure = error;
                                stopped.abort(error);
                            })
                                .finally(() => pending.delete(task));
                            pending.set(task, generation);
                        }
                    }
                    catch (error) {
                        if (!(stopped.signal.aborted &&
                            error instanceof Error &&
                            error.name === 'AbortError')) {
                            failure ??= error;
                        }
                    }
                    finally {
                        for (const generation of pending.values())
                            generation.abort(stopped.signal.reason);
                        await Promise.allSettled(pending.keys());
                        signal?.removeEventListener('abort', abort);
                    }
                    if (failure !== undefined)
                        throw failure;
                })();
                void running.catch(() => { });
                const stop = async () => {
                    stopped.abort();
                    await running;
                };
                Object.defineProperty(stop, 'done', { value: running, enumerable: true });
                return stop;
            },
            list: async (path) => expect(await session.call('filesystem_list', { path }), 'entries'),
            listPage: async (path, { after = null, observed = null, limit = 256 } = {}) => {
                if ((after === null) !== (observed === null)) {
                    throw new TypeError('filesystem directory continuation requires both after and observed');
                }
                const page = expect(await session.call('filesystem_list_page', { path, after, observed, limit }), 'directory_page');
                const identityValid = Boolean(page.identity) && new TextEncoder().encode(page.identity).byteLength <= 256;
                const structureValid = page.entries.length <= limit &&
                    !(page.more && !page.next) &&
                    !(page.more && page.entries.length === 0) &&
                    !(after !== null && page.next === after) &&
                    !page.entries.some((entry) => !isDirectFilesystemChild(path, entry.path)) &&
                    !page.entries.some((entry, index) => {
                        const previous = index === 0 ? after : page.entries[index - 1].path;
                        return previous !== null && compareUtf8(entry.path, previous) <= 0;
                    }) &&
                    !(page.entries.length > 0 && page.next !== page.entries.at(-1).path);
                if (observed !== null && identityValid && structureValid && page.identity !== observed) {
                    throw new DirectoryIdentityChangedError(path, observed, page.identity, after);
                }
                if (!identityValid ||
                    (observed !== null && page.identity !== observed) ||
                    !structureValid) {
                    throw new TypeError('host returned an inconsistent filesystem directory page');
                }
                return page;
            },
            walk: async function* (path, { pageSize = 256, signal } = {}) {
                exactFilesystemPageSize(pageSize);
                const stack = [
                    { path, after: null, observed: null, entries: [], index: 0, more: true },
                ];
                while (stack.length > 0) {
                    const current = stack.at(-1);
                    if (current.index < current.entries.length) {
                        const entry = current.entries[current.index++];
                        const child = entry.directory ? entry.path : null;
                        yield entry;
                        if (child !== null) {
                            stack.push({
                                path: child,
                                after: null,
                                observed: null,
                                entries: [],
                                index: 0,
                                more: true,
                            });
                        }
                        continue;
                    }
                    if (!current.more) {
                        stack.pop();
                        continue;
                    }
                    requireFilesystemActive(signal);
                    const page = await api.files.listPage(current.path, {
                        after: current.after,
                        observed: current.observed,
                        limit: pageSize,
                    });
                    requireFilesystemActive(signal);
                    current.observed ??= page.identity;
                    current.entries = page.entries;
                    current.index = 0;
                    current.more = page.more;
                    current.after = page.next;
                }
            },
            read: async (path) => expect(await session.call('filesystem_read', { path }), 'contents'),
            readRange: async (path, offset = 0, limit = 65536, observed = null) => {
                const [boundedOffset, boundedLimit] = exactFileRange(offset, limit);
                const range = expect(await session.call('filesystem_read_range', {
                    path,
                    offset: boundedOffset,
                    limit: boundedLimit,
                    observed,
                }), 'file_range');
                const complete = range.offset >= range.total || range.contents.length >= range.total - range.offset;
                if (observed !== null &&
                    range.path === path &&
                    range.identity &&
                    new TextEncoder().encode(range.identity).byteLength <= 256 &&
                    range.identity !== observed) {
                    throw new FileIdentityChangedError(path, observed, range.identity, boundedOffset);
                }
                if (range.path !== path ||
                    !range.identity ||
                    new TextEncoder().encode(range.identity).byteLength > 256 ||
                    range.offset !== boundedOffset ||
                    range.contents.length > boundedLimit ||
                    range.eof !== complete ||
                    range.truncated === range.eof ||
                    (!range.eof && range.contents.length === 0)) {
                    throw new TypeError('host returned an inconsistent filesystem file range');
                }
                return range;
            },
            readRanges: async (ranges) => {
                if (!Array.isArray(ranges) || ranges.length < 1 || ranges.length > 64)
                    throw new RangeError('filesystem range batch must contain 1 through 64 ranges');
                let aggregate = 0;
                const exact = ranges.map(({ path, offset = 0, limit = 65536, observed = null }) => {
                    const [boundedOffset, boundedLimit] = exactFileRange(offset, limit);
                    aggregate += boundedLimit;
                    return { path, offset: boundedOffset, limit: boundedLimit, observed };
                });
                if (aggregate > 65536)
                    throw new RangeError('filesystem range batch exceeds 65536 requested bytes');
                const values = expect(await session.call('filesystem_read_ranges', { ranges: exact }), 'file_ranges');
                values.forEach((value, index) => {
                    const requested = exact[index];
                    if (requested &&
                        requested.observed !== null &&
                        value.path === requested.path &&
                        value.identity &&
                        new TextEncoder().encode(value.identity).byteLength <= 256 &&
                        value.identity !== requested.observed) {
                        throw new FileIdentityChangedError(requested.path, requested.observed, value.identity, requested.offset);
                    }
                });
                const files = new Map();
                if (values.length !== exact.length ||
                    values.some((value, index) => {
                        const prior = files.get(value.path);
                        const inconsistentFile = prior !== undefined &&
                            (prior.identity !== value.identity || prior.total !== value.total);
                        files.set(value.path, { identity: value.identity, total: value.total });
                        return (value.path !== exact[index].path ||
                            !value.identity ||
                            new TextEncoder().encode(value.identity).byteLength > 256 ||
                            (exact[index].observed !== null && value.identity !== exact[index].observed) ||
                            value.offset !== exact[index].offset ||
                            value.contents.length > exact[index].limit ||
                            value.eof !==
                                (value.offset >= value.total ||
                                    value.contents.length >= value.total - value.offset) ||
                            value.truncated === value.eof ||
                            (!value.eof && value.contents.length === 0) ||
                            inconsistentFile);
                    }))
                    throw new TypeError('host returned an inconsistent filesystem range batch');
                return values;
            },
            readChunks: async function* (path, { offset = 0, chunkBytes = 65_536, observed = null, signal, } = {}) {
                const [start, limit] = exactFileRange(offset, chunkBytes);
                let cursor = start;
                let identity = observed;
                let total;
                for (;;) {
                    requireFilesystemActive(signal);
                    const range = await api.files.readRange(path, cursor, limit, identity);
                    requireFilesystemActive(signal);
                    identity ??= range.identity;
                    if (range.identity !== identity) {
                        throw new FileIdentityChangedError(path, identity, range.identity, cursor);
                    }
                    total ??= range.total;
                    if (range.total !== total) {
                        throw new FileExtentChangedError(path, identity, total, range.total, cursor);
                    }
                    const eof = range.eof;
                    const next = cursor + range.contents.length;
                    yield range;
                    if (eof)
                        return;
                    cursor = next;
                }
            },
            readText: async (path, { maxBytes, chunkBytes = 65_536, observed = null, signal, }) => {
                if (!Number.isSafeInteger(maxBytes) || maxBytes < 1 || maxBytes > 64 * 1024 * 1024) {
                    throw new RangeError('filesystem text maxBytes must be an integer between 1 and 67108864');
                }
                const [, limit] = exactFileRange(0, chunkBytes);
                const decoder = new TextDecoder('utf-8', { fatal: true });
                const parts = [];
                let bytes = 0;
                let identity = observed;
                try {
                    for await (const range of api.files.readChunks(path, {
                        chunkBytes: limit,
                        observed,
                        signal,
                    })) {
                        identity ??= range.identity;
                        if (range.total > maxBytes || bytes + range.contents.length > maxBytes) {
                            throw new FileTextLimitError(path, range.identity, range.total, maxBytes);
                        }
                        bytes += range.contents.length;
                        parts.push(decoder.decode(Uint8Array.from(range.contents), { stream: !range.eof }));
                    }
                    parts.push(decoder.decode());
                }
                catch (error) {
                    if (identity &&
                        error instanceof TypeError &&
                        /encoded data was not valid/i.test(error.message)) {
                        throw new FileTextDecodeError(path, identity, bytes, error);
                    }
                    throw error;
                }
                if (!identity)
                    throw new TypeError('host returned a filesystem file without an identity');
                return { text: parts.join(''), identity, bytes };
            },
            stat: async (path) => {
                const entry = expect(await session.call('filesystem_stat', { path }), 'entry');
                if (entry.path !== path) {
                    throw new TypeError(`host returned filesystem metadata for ${entry.path}, expected ${path}; no file identity was assumed`);
                }
                return entry;
            },
            write: (path, contents) => done('filesystem_write', { path, contents: exactFileContents(contents) }),
            writeObserved: async (path, observed, contents) => expect(await session.call('filesystem_write_observed', {
                path,
                observed,
                contents: exactFileContents(contents),
            }), 'identity'),
            createObserved: async (path, contents) => expect(await session.call('filesystem_create_observed', {
                path,
                contents: exactFileContents(contents),
            }), 'identity'),
            mkdir: (path) => done('filesystem_mkdir', { path }),
            rename: (from, to) => done('filesystem_rename', { from, to }),
            renameObserved: async (from, to, observed) => expect(await session.call('filesystem_rename_observed', { from, to, observed }), 'identity'),
            remove: (path) => done('filesystem_remove', { path }),
            removeObserved: (path, observed) => done('filesystem_remove_observed', { path, observed }),
        },
        state: {
            read: async () => expect(await session.call('state_read', undefined), 'state'),
            write: async (observed, contents) => expect(await session.call('state_write', {
                observed: exactStateIdentity(observed),
                contents: exactStateBytes(contents),
            }), 'identity'),
            clear: (observed) => done('state_clear', { observed: exactStateIdentity(observed) }),
            readJson: async (codec) => {
                const checked = exactStateCodec(codec);
                return decodeJsonState(await api.state.read(), checked);
            },
            writeJson: (observed, value, codec) => api.state.write(observed, encodeJsonState(value, codec)),
            updateJson: async (codec, update, { attempts = 4, signal: updateSignal } = {}) => {
                if (!Number.isSafeInteger(attempts) || attempts < 1 || attempts > 16)
                    throw new RangeError('extension state JSON update attempts must be an integer from 1 through 16');
                if (typeof update !== 'function')
                    throw new TypeError('extension state JSON update requires an update function');
                for (let attempt = 0; attempt < attempts; attempt += 1) {
                    requireStateUpdateActive(updateSignal);
                    const current = await api.state.readJson(codec);
                    requireStateUpdateActive(updateSignal);
                    const value = await update(current.value);
                    requireStateUpdateActive(updateSignal);
                    try {
                        const identity = await api.state.writeJson(current.identity, value, codec);
                        return { identity, value };
                    }
                    catch (error) {
                        if (!(error instanceof ExtensionError) ||
                            error.kind !== 'conflict' ||
                            attempt + 1 === attempts)
                            throw error;
                    }
                }
                throw new Error('unreachable extension state update attempt');
            },
        },
        preferences: {
            read: async () => {
                const preferences = expect(await session.call('preference_read', undefined), 'preferences');
                if (new Set(preferences.entries.map(([key]) => key)).size !== preferences.entries.length) {
                    throw new TypeError('host returned duplicate preference keys; no preference state was assumed');
                }
                return preferences;
            },
            set: async (observed, key, value) => expect(await session.call('preference_set', { observed, key, value }), 'revision'),
            remove: async (observed, key) => expect(await session.call('preference_remove', { observed, key }), 'revision'),
        },
        credentials: {
            read: async (key) => {
                const exactKey = exactCredentialKey(key);
                const credential = expect(await session.call('credential_read', { key: exactKey }), 'credential');
                if (credential.key !== exactKey) {
                    throw new TypeError(`host returned credential ${credential.key}, expected ${exactKey}; no credential value was assumed`);
                }
                return credential;
            },
            set: async (observed, key, value) => expect(await session.call('credential_set', {
                observed,
                key: exactCredentialKey(key),
                value: exactCredentialBytes(value),
            }), 'revision'),
            remove: async (observed, key) => expect(await session.call('credential_remove', { observed, key: exactCredentialKey(key) }), 'revision'),
        },
        subscribe,
        unsubscribe,
    };
    Object.defineProperty(api, 'withSignal', {
        value: (nextSignal) => workspace(hostSession, { signal: nextSignal }),
        enumerable: false,
    });
    const watch = async (topic, snapshot, listener, label) => {
        if (typeof listener !== 'function')
            throw new TypeError(`${label} listener must be a function`);
        const off = hostSession.onEvent((event) => {
            if ('snapshot' in event && event.snapshot === snapshot)
                return listener(event.of);
        });
        try {
            await subscribe(topic);
        }
        catch (error) {
            off();
            throw error;
        }
        let stopping;
        const stop = () => (stopping ??= (async () => {
            signal?.removeEventListener('abort', onAbort);
            off();
            await unsubscribe(topic);
        })());
        const onAbort = () => {
            void stop();
        };
        signal?.addEventListener('abort', onAbort, { once: true });
        if (signal?.aborted)
            await stop();
        return stop;
    };
    const providerCatalogue = (extensions) => {
        if (!Array.isArray(extensions) ||
            extensions.some((extension) => typeof extension.enabled !== 'boolean' || !Array.isArray(extension.pane_providers))) {
            throw new Error('host does not expose installed provider declarations');
        }
        const all = extensions
            .filter(({ enabled }) => enabled)
            .flatMap((extension) => extension.pane_providers.map((provider) => ({
            extension: extension.name,
            image_digest: extension.image_digest,
            version: extension.version ?? '',
            status: extension.status,
            id: provider.id,
            title: provider.title,
            icon: provider.icon ?? null,
        })));
        return { providers: all.slice(0, 200), truncated: all.length > 200 };
    };
    api.extensions.providers = async () => providerCatalogue(await api.extensions.list());
    api.extensions.waitForProviders = async (after, { timeoutMs = 30_000 } = {}) => {
        if (after == null ||
            typeof after.name !== 'string' ||
            typeof after.image_digest !== 'string' ||
            typeof after.status !== 'string') {
            throw new TypeError('provider catalogue wait requires an exact extension name, image digest, and status cursor');
        }
        if (!Number.isSafeInteger(timeoutMs) || timeoutMs < 1 || timeoutMs > 30_000)
            throw new RangeError('provider catalogue wait timeout must be between 1 and 30000ms');
        let dispose;
        let timer;
        let settled = false;
        return new Promise((resolve, reject) => {
            const finish = (value, error) => {
                if (settled)
                    return;
                settled = true;
                clearTimeout(timer);
                Promise.resolve(dispose?.()).then(() => (error ? reject(error) : resolve(value)), reject);
            };
            api
                .watchExtensions((extensions) => {
                const current = extensions.find(({ name }) => name === after.name);
                if (current?.image_digest === after.image_digest && current.status === after.status)
                    return;
                try {
                    finish({
                        changed: true,
                        extension: current == null
                            ? null
                            : {
                                name: current.name,
                                image_digest: current.image_digest,
                                status: current.status,
                            },
                        catalogue: providerCatalogue(extensions),
                    });
                }
                catch (error) {
                    finish(undefined, error);
                }
            })
                .then((stop) => {
                dispose = stop;
                if (settled)
                    void stop();
            }, (error) => finish(undefined, error));
            timer = setTimeout(() => finish({ changed: false, after }), timeoutMs);
        });
    };
    api.watchContainers = (listener) => watch('containers', 'containers', listener, 'container');
    api.watchContainerInventory = (listener) => watch('container-inventory', 'container_inventory', listener, 'container inventory');
    api.containers.startAndWait = async (id, generation, { timeoutMs = 30_000 } = {}) => {
        const identity = immutableIdentity(id, [32, 64], 'container');
        if (!Number.isSafeInteger(timeoutMs) || timeoutMs < 1 || timeoutMs > 30_000) {
            throw new RangeError('container start wait timeout must be between 1 and 30000ms');
        }
        let sequence = 0;
        let baseline = 0;
        let started = false;
        let observed;
        let timer;
        const running = new Promise((resolve) => {
            observed = (containers) => {
                sequence += 1;
                if (!started || sequence <= baseline)
                    return;
                const current = containers.find((container) => container.id === identity);
                if (current?.state === 'running')
                    resolve(current);
            };
        });
        const stop = await api.watchContainers(observed);
        baseline = sequence;
        try {
            started = true;
            await api.containers.start(identity, generation);
            const container = await Promise.race([
                running,
                new Promise((resolve) => {
                    timer = setTimeout(() => resolve(null), timeoutMs);
                }),
            ]);
            return container === null
                ? { changed: false, id: identity, state: 'running' }
                : { changed: true, container };
        }
        finally {
            clearTimeout(timer);
            await stop();
        }
    };
    api.containers.stopAndWait = async (id, generation, { timeoutMs = 30_000 } = {}) => {
        const identity = immutableIdentity(id, [32, 64], 'container');
        if (!Number.isSafeInteger(timeoutMs) || timeoutMs < 1 || timeoutMs > 30_000) {
            throw new RangeError('container stop wait timeout must be between 1 and 30000ms');
        }
        let sequence = 0;
        let baseline = 0;
        let stopped = false;
        let observed;
        let timer;
        const exited = new Promise((resolve) => {
            observed = (containers) => {
                sequence += 1;
                if (!stopped || sequence <= baseline)
                    return;
                const current = containers.find((container) => container.id === identity);
                if (current?.state === 'exited')
                    resolve(current);
            };
        });
        const stopWatching = await api.watchContainers(observed);
        baseline = sequence;
        try {
            stopped = true;
            await api.containers.stop(identity, generation);
            const container = await Promise.race([
                exited,
                new Promise((resolve) => {
                    timer = setTimeout(() => resolve(null), timeoutMs);
                }),
            ]);
            return container === null
                ? { changed: false, id: identity, state: 'exited' }
                : { changed: true, container };
        }
        finally {
            clearTimeout(timer);
            await stopWatching();
        }
    };
    api.containers.removeAndWait = async (id, generation, { timeoutMs = 30_000 } = {}) => {
        const identity = immutableIdentity(id, [32, 64], 'container');
        if (!Number.isSafeInteger(timeoutMs) || timeoutMs < 1 || timeoutMs > 30_000)
            throw new RangeError('container remove wait timeout must be between 1 and 30000ms');
        let sequence = 0;
        let baseline = 0;
        let removing = false;
        let observed;
        let timer;
        const absent = new Promise((resolve) => {
            observed = (inventory) => {
                sequence += 1;
                if (!removing || sequence <= baseline || !inventory.complete)
                    return;
                if (!inventory.containers.some((container) => container.id === identity))
                    resolve();
            };
        });
        const stopWatching = await api.watchContainerInventory(observed);
        baseline = sequence;
        try {
            removing = true;
            await api.containers.remove(identity, generation);
            const removed = await Promise.race([
                absent.then(() => true),
                new Promise((resolve) => {
                    timer = setTimeout(() => resolve(false), timeoutMs);
                }),
            ]);
            return { changed: removed, id: identity };
        }
        finally {
            clearTimeout(timer);
            await stopWatching();
        }
    };
    api.containers.restartAndWait = async (id, generation, { timeoutMs = 30_000 } = {}) => {
        const identity = immutableIdentity(id, [32, 64], 'container');
        if (!Number.isSafeInteger(generation) || generation < 0)
            throw new TypeError('container restart wait requires an observed nonnegative safe generation');
        if (!Number.isSafeInteger(timeoutMs) || timeoutMs < 1 || timeoutMs > 30_000)
            throw new RangeError('container restart wait timeout must be between 1 and 30000ms');
        let observed;
        let timer;
        const restarted = new Promise((resolve) => {
            observed = (containers) => {
                const current = containers.find((container) => container.id === identity);
                if (current?.state === 'running' && current.generation > generation)
                    resolve(current);
            };
        });
        const stopWatching = await api.watchContainers(observed);
        try {
            await api.containers.restart(identity, generation);
            const container = await Promise.race([
                restarted,
                new Promise((resolve) => {
                    timer = setTimeout(() => resolve(null), timeoutMs);
                }),
            ]);
            return container === null
                ? { changed: false, id: identity, generation }
                : { changed: true, container };
        }
        finally {
            clearTimeout(timer);
            await stopWatching();
        }
    };
    api.watchImageInventory = (listener) => watch('images', 'images', listener, 'image inventory');
    api.watchImages = (listener) => api.watchImageInventory((inventory) => listener(inventory.images));
    api.watchVolumeInventory = (listener) => watch('volumes', 'volumes', listener, 'volume inventory');
    api.watchVolumes = (listener) => api.watchVolumeInventory((inventory) => listener(inventory.volumes));
    api.watchNetworkInventory = (listener) => watch('networks', 'networks', (value) => {
        const inventory = value;
        exactNetworks(inventory.networks);
        return listener(inventory);
    }, 'network inventory');
    api.watchNetworks = (listener) => api.watchNetworkInventory((inventory) => listener(inventory.networks));
    api.images.removeAndWait = async (reference, { timeoutMs = 30_000 } = {}) => {
        const digest = immutableDigest(reference, 'image');
        if (!Number.isSafeInteger(timeoutMs) || timeoutMs < 1 || timeoutMs > 30_000)
            throw new RangeError('image remove wait timeout must be between 1 and 30000ms');
        let removing = false;
        let timer;
        let finish;
        const absent = new Promise((resolve) => {
            finish = resolve;
        });
        const stopWatching = await api.watchImageInventory((inventory) => {
            if (removing &&
                !inventory.truncated &&
                !inventory.images.some((image) => image.id === digest))
                finish();
        });
        try {
            removing = true;
            await api.images.remove(digest);
            const changed = await Promise.race([
                absent.then(() => true),
                new Promise((resolve) => {
                    timer = setTimeout(() => resolve(false), timeoutMs);
                }),
            ]);
            return { changed, id: digest };
        }
        finally {
            clearTimeout(timer);
            await stopWatching();
        }
    };
    api.volumes.removeAndWait = async (name, generation, { timeoutMs = 30_000 } = {}) => {
        const identity = immutableIdentity(generation, [32], 'volume generation');
        if (!Number.isSafeInteger(timeoutMs) || timeoutMs < 1 || timeoutMs > 30_000)
            throw new RangeError('volume remove wait timeout must be between 1 and 30000ms');
        let removing = false;
        let timer;
        let finish;
        const absent = new Promise((resolve) => {
            finish = resolve;
        });
        const stopWatching = await api.watchVolumeInventory((inventory) => {
            if (removing &&
                !inventory.truncated &&
                !inventory.volumes.some((volume) => volume.name === name && volume.generation === identity))
                finish();
        });
        try {
            removing = true;
            await api.volumes.remove(name, identity);
            const changed = await Promise.race([
                absent.then(() => true),
                new Promise((resolve) => {
                    timer = setTimeout(() => resolve(false), timeoutMs);
                }),
            ]);
            return { changed, name, generation: identity };
        }
        finally {
            clearTimeout(timer);
            await stopWatching();
        }
    };
    api.networks.removeAndWait = async (reference, { timeoutMs = 30_000 } = {}) => {
        const identity = immutableIdentity(reference, [32], 'network');
        if (!Number.isSafeInteger(timeoutMs) || timeoutMs < 1 || timeoutMs > 30_000)
            throw new RangeError('network remove wait timeout must be between 1 and 30000ms');
        let removing = false;
        let timer;
        let finish;
        const absent = new Promise((resolve) => {
            finish = resolve;
        });
        const stopWatching = await api.watchNetworkInventory((inventory) => {
            if (removing &&
                !inventory.truncated &&
                !inventory.networks.some((network) => network.id === identity))
                finish();
        });
        try {
            removing = true;
            await api.networks.remove(identity);
            const changed = await Promise.race([
                absent.then(() => true),
                new Promise((resolve) => {
                    timer = setTimeout(() => resolve(false), timeoutMs);
                }),
            ]);
            return { changed, id: identity };
        }
        finally {
            clearTimeout(timer);
            await stopWatching();
        }
    };
    api.watchTerminal = (listener) => watch('terminal', 'terminal', (tabs) => listener(exactTabs(tabs)), 'terminal');
    api.watchPaneChanges = (listener) => watch('pane-changes', 'pane_changes', listener, 'pane change');
    api.terminal.readAllStable = async ({ lines, attempts = 3, signal: readSignal } = {}) => {
        if (!Number.isSafeInteger(attempts) || attempts < 1 || attempts > 16) {
            throw new RangeError('stable pane inventory attempts must be an integer within 1..=16');
        }
        if (lines !== undefined && (!Number.isSafeInteger(lines) || lines < 0)) {
            throw new TypeError('stable pane inventory lines must be a nonnegative safe integer');
        }
        if (readSignal?.aborted)
            throw outputAbort(readSignal);
        const scoped = readSignal === undefined ? api : workspace(hostSession, { signal: readSignal });
        const samePane = (left, right) => left.slot === right.slot &&
            left.generation === right.generation &&
            left.revision === right.revision &&
            left.kind === right.kind &&
            left.provider?.extension === right.provider?.extension &&
            left.provider?.provider === right.provider?.provider &&
            left.tab === right.tab &&
            left.title === right.title &&
            left.focused === right.focused;
        for (let attempt = 1; attempt <= attempts; attempt += 1) {
            let readable;
            try {
                readable = await scoped.terminal.readAll({ lines });
            }
            catch (error) {
                if (error instanceof PaneChangedError && attempt < attempts)
                    continue;
                throw error;
            }
            if (!readable.complete) {
                throw new IncompletePaneInventoryError(readable.panes.map(({ pane }) => pane));
            }
            const before = {
                panes: readable.panes.map(({ pane }) => pane),
                truncated: !readable.complete,
            };
            const after = await scoped.terminal.panes();
            const unchanged = before.truncated === after.truncated &&
                before.panes.length === after.panes.length &&
                before.panes.every((pane, index) => samePane(pane, after.panes[index]));
            if (unchanged)
                return readable;
            if (attempt === attempts)
                throw new PaneInventoryChangedError(attempts, before, after);
        }
        throw new Error('unreachable stable pane inventory attempt');
    };
    api.paneChanges = async function* ({ signal: iteratorSignal } = {}) {
        if (iteratorSignal?.aborted)
            throw outputAbort(iteratorSignal);
        let pending;
        let wake;
        let subscribed = false;
        const off = hostSession.onEvent((event) => {
            if (!('snapshot' in event) || event.snapshot !== 'pane_changes')
                return;
            return new Promise((acknowledge) => {
                pending = { value: event.of, acknowledge };
                wake?.();
            });
        });
        let abortIterator;
        const aborted = new Promise((_, reject) => {
            abortIterator = () => reject(outputAbort(iteratorSignal));
            iteratorSignal?.addEventListener('abort', abortIterator, { once: true });
        });
        try {
            await subscribe('pane-changes');
            subscribed = true;
            for (;;) {
                if (!pending) {
                    await Promise.race([
                        new Promise((resolve) => {
                            wake = resolve;
                        }),
                        hostSession.closed.then((error) => Promise.reject(error)),
                        aborted,
                    ]);
                    wake = undefined;
                }
                const delivery = pending;
                pending = undefined;
                yield delivery.value;
                delivery.acknowledge();
            }
        }
        finally {
            pending?.acknowledge();
            iteratorSignal?.removeEventListener('abort', abortIterator);
            off();
            if (subscribed)
                await unsubscribe('pane-changes').catch(() => { });
        }
    };
    api.terminal.waitForText = async (slot, after, { lines, timeoutMs = 30_000, signal } = {}) => {
        if (typeof slot !== 'string' || slot.length === 0)
            throw new TypeError('pane text wait requires a nonempty slot');
        if (after == null ||
            !Number.isSafeInteger(after.generation) ||
            after.generation < 0 ||
            !Number.isSafeInteger(after.revision) ||
            after.revision < 0) {
            throw new TypeError('pane text wait requires an exact nonnegative generation and revision cursor');
        }
        if (!Number.isSafeInteger(timeoutMs) || timeoutMs < 1 || timeoutMs > 30_000) {
            throw new RangeError('pane text wait timeout must be between 1 and 30000ms');
        }
        if (signal?.aborted)
            throw outputAbort(signal);
        let dispose;
        let timer;
        let abort;
        let settled = false;
        let reading = false;
        let pending = false;
        return new Promise((resolve, reject) => {
            const finish = (value, error) => {
                if (settled)
                    return;
                settled = true;
                clearTimeout(timer);
                if (abort)
                    signal?.removeEventListener('abort', abort);
                Promise.resolve(dispose?.()).then(() => (error ? reject(error) : resolve(value)), reject);
            };
            const reconcile = () => {
                pending = true;
                if (reading)
                    return;
                reading = true;
                void (async () => {
                    try {
                        while (pending && !settled) {
                            pending = false;
                            const readable = await api.terminal.toText(slot, { lines });
                            const cursor = readable.snapshot;
                            if (cursor.generation === after.generation && cursor.revision === after.revision)
                                continue;
                            finish({ changed: true, readable });
                        }
                    }
                    catch (error) {
                        if (error instanceof PaneChangedError && error.slot === slot && !settled) {
                            pending = true;
                        }
                        else {
                            finish(undefined, error);
                        }
                    }
                    finally {
                        reading = false;
                        if (pending && !settled)
                            reconcile();
                    }
                })();
            };
            const observe = (change) => {
                if (settled ||
                    change.slot !== slot ||
                    (change.generation === after.generation && change.revision === after.revision))
                    return;
                reconcile();
            };
            abort = () => finish(undefined, outputAbort(signal));
            signal?.addEventListener('abort', abort, { once: true });
            api.watchPaneChanges(observe).then((stop) => {
                dispose = stop;
                if (settled)
                    void stop();
                else
                    reconcile();
            }, (error) => finish(undefined, error));
            timer = setTimeout(() => finish({ changed: false, after }), timeoutMs);
        });
    };
    const writeAndWait = async (slot, generation, revision, input, { lines, timeoutMs = 30_000, signal, } = {}, projectReadable = false) => {
        if (typeof slot !== 'string' || slot.length === 0)
            throw new TypeError('terminal input wait requires a nonempty slot');
        if (!Number.isSafeInteger(generation) ||
            generation < 0 ||
            !Number.isSafeInteger(revision) ||
            revision < 0) {
            throw new TypeError('terminal input wait requires nonnegative safe integer generation and revision');
        }
        const contents = exactPaneInput(input);
        if (lines !== undefined && (!Number.isSafeInteger(lines) || lines < 0)) {
            throw new TypeError('terminal input wait lines must be a nonnegative safe integer');
        }
        if (!Number.isSafeInteger(timeoutMs) || timeoutMs < 1 || timeoutMs > 30_000) {
            throw new RangeError('terminal input wait timeout must be between 1 and 30000ms');
        }
        if (signal?.aborted)
            throw outputAbort(signal);
        const scoped = signal ? api.withSignal(signal) : api;
        let announced;
        let wake;
        const stop = await scoped.watchPaneChanges((change) => {
            if (change.slot === slot &&
                (change.generation !== generation || change.revision !== revision))
                if (wake) {
                    const resolve = wake;
                    wake = undefined;
                    resolve(change);
                }
                else {
                    announced = change;
                }
        });
        let timer;
        let abort;
        try {
            const before = await scoped.terminal.read(slot, lines);
            if (before.generation !== generation || before.revision !== revision) {
                throw new Error('terminal screen cursor changed before input authority');
            }
            await scoped.terminal.writeInput(slot, generation, revision, contents);
            const deadline = Date.now() + timeoutMs;
            const aborted = new Promise((_, reject) => {
                abort = () => reject(outputAbort(signal));
                signal?.addEventListener('abort', abort, { once: true });
            });
            for (;;) {
                if (!announced) {
                    const remaining = deadline - Date.now();
                    if (remaining < 1)
                        return { changed: false, before };
                    const change = await Promise.race([
                        new Promise((resolve) => {
                            wake = resolve;
                        }),
                        new Promise((resolve) => {
                            timer = setTimeout(() => resolve(null), remaining);
                        }),
                        aborted,
                    ]);
                    clearTimeout(timer);
                    if (change === null)
                        return { changed: false, before };
                }
                else {
                    announced = undefined;
                }
                const readable = projectReadable
                    ? await scoped.terminal.toText(slot, { lines })
                    : undefined;
                const after = readable?.snapshot ?? (await scoped.terminal.read(slot, lines));
                if (after.generation === generation && after.revision === revision)
                    continue;
                if (!projectReadable && after.generation !== generation) {
                    throw new Error('terminal pane was replaced before input result could be verified');
                }
                return { changed: true, before, after: readable ?? after };
            }
        }
        finally {
            clearTimeout(timer);
            if (abort)
                signal?.removeEventListener('abort', abort);
            await stop();
        }
    };
    api.terminal.writeAndWait = (slot, generation, revision, input, options) => writeAndWait(slot, generation, revision, input, options);
    api.terminal.writeObservedAndWait = (before, input, options) => {
        if (!before ||
            typeof before.slot !== 'string' ||
            !Number.isSafeInteger(before.generation) ||
            !Number.isSafeInteger(before.revision)) {
            throw new TypeError('observed terminal input requires a snapshot with an exact cursor');
        }
        return writeAndWait(before.slot, before.generation, before.revision, input, options);
    };
    api.terminal.writeObservedAndWaitForText = (before, input, options) => {
        if (!before ||
            typeof before.slot !== 'string' ||
            !Number.isSafeInteger(before.generation) ||
            !Number.isSafeInteger(before.revision)) {
            throw new TypeError('observed terminal input requires a snapshot with an exact cursor');
        }
        return writeAndWait(before.slot, before.generation, before.revision, input, options, true);
    };
    api.terminal.writeObservedAndWaitForQuietText = async (before, input, { lines, quietMs = 150, timeoutMs = 30_000, signal } = {}) => {
        if (!Number.isSafeInteger(quietMs) || quietMs < 1 || quietMs > 30_000) {
            throw new RangeError('terminal quiet window must be between 1 and 30000ms');
        }
        if (!Number.isSafeInteger(timeoutMs) || timeoutMs < 1 || timeoutMs > 30_000) {
            throw new RangeError('terminal quiet wait timeout must be between 1 and 30000ms');
        }
        const deadline = Date.now() + timeoutMs;
        const first = await api.terminal.writeObservedAndWaitForText(before, input, {
            lines,
            timeoutMs,
            signal,
        });
        if (!first.changed)
            return { ...first, settled: false, replaced: false };
        let after = first.after;
        if (after.snapshot.generation !== before.generation) {
            return { ...first, after, settled: false, replaced: true };
        }
        for (;;) {
            const remaining = deadline - Date.now();
            if (remaining < 1)
                return { ...first, after, settled: false, replaced: false };
            const window = Math.min(quietMs, remaining);
            const next = await api.terminal.waitForText(after.snapshot.slot, after.snapshot, {
                lines,
                timeoutMs: window,
                signal,
            });
            if (!next.changed) {
                return { ...first, after, settled: window === quietMs, replaced: false };
            }
            after = next.readable;
            if (after.snapshot.generation !== before.generation) {
                return { ...first, after, settled: false, replaced: true };
            }
        }
    };
    api.terminal.spawnAndWait = async (slot, generation, revision, command, { lines, timeoutMs = 30_000 } = {}) => {
        if (typeof slot !== 'string' || slot.length === 0)
            throw new TypeError('terminal spawn wait requires a nonempty slot');
        if (!Number.isSafeInteger(generation) ||
            generation < 0 ||
            !Number.isSafeInteger(revision) ||
            revision < 0) {
            throw new TypeError('terminal spawn wait requires nonnegative safe integer generation and revision');
        }
        const argv = [...exactCommand(command)];
        if (lines !== undefined && (!Number.isSafeInteger(lines) || lines < 0)) {
            throw new TypeError('terminal spawn wait lines must be a nonnegative safe integer');
        }
        if (!Number.isSafeInteger(timeoutMs) || timeoutMs < 1 || timeoutMs > 30_000) {
            throw new RangeError('terminal spawn wait timeout must be between 1 and 30000ms');
        }
        let changed;
        const observed = new Promise((resolve) => {
            changed = resolve;
        });
        const stop = await api.watchPaneChanges((change) => {
            if (change.slot === slot &&
                (change.generation !== generation || change.revision !== revision))
                changed(change);
        });
        let timer;
        try {
            const before = await api.terminal.read(slot, lines);
            if (before.generation !== generation || before.revision !== revision) {
                throw new Error('terminal screen cursor changed before spawn authority');
            }
            await api.terminal.spawnObserved(slot, generation, revision, argv);
            const change = await Promise.race([
                observed,
                new Promise((resolve) => {
                    timer = setTimeout(() => resolve(null), timeoutMs);
                }),
            ]);
            if (change === null)
                return { changed: false, command: argv, before };
            const after = await api.terminal.read(slot, lines);
            if (after.generation === generation && after.revision === revision) {
                throw new Error('pane change did not advance the terminal screen cursor after spawn');
            }
            return { changed: true, command: argv, before, after };
        }
        finally {
            clearTimeout(timer);
            await stop();
        }
    };
    api.terminal.resizeGridAndWait = async (slot, generation, revision, columns, rows, { lines, timeoutMs = 30_000 } = {}) => {
        if (typeof slot !== 'string' || slot.length === 0)
            throw new TypeError('terminal resize wait requires a nonempty slot');
        if (!Number.isSafeInteger(generation) ||
            generation < 0 ||
            !Number.isSafeInteger(revision) ||
            revision < 0) {
            throw new TypeError('terminal resize wait requires nonnegative safe integer generation and revision');
        }
        if (!Number.isInteger(columns) ||
            !Number.isInteger(rows) ||
            columns < 1 ||
            rows < 1 ||
            columns > 1000 ||
            rows > 1000) {
            throw new RangeError('terminal grid rows and columns must be integers within 1..=1000');
        }
        if (lines !== undefined && (!Number.isSafeInteger(lines) || lines < 0)) {
            throw new TypeError('terminal resize wait lines must be a nonnegative safe integer');
        }
        if (!Number.isSafeInteger(timeoutMs) || timeoutMs < 1 || timeoutMs > 30_000) {
            throw new RangeError('terminal resize wait timeout must be between 1 and 30000ms');
        }
        let changed;
        const observed = new Promise((resolve) => {
            changed = resolve;
        });
        const stop = await api.watchPaneChanges((change) => {
            if (change.slot === slot &&
                (change.generation !== generation || change.revision !== revision))
                changed(change);
        });
        let timer;
        try {
            const before = await api.terminal.read(slot, lines);
            if (before.generation !== generation || before.revision !== revision) {
                throw new Error('terminal screen cursor changed before resize authority');
            }
            await api.terminal.resizeGridObserved(slot, generation, revision, columns, rows);
            const change = await Promise.race([
                observed,
                new Promise((resolve) => {
                    timer = setTimeout(() => resolve(null), timeoutMs);
                }),
            ]);
            if (change === null)
                return { changed: false, columns, rows, before };
            const after = await api.terminal.read(slot, lines);
            if (after.generation !== generation)
                throw new Error('resized pane slot was replaced before verification');
            if (after.revision === revision || after.columns !== columns || after.rows !== rows) {
                throw new Error('pane changed without applying the requested terminal grid');
            }
            return { changed: true, columns, rows, before, after };
        }
        finally {
            clearTimeout(timer);
            await stop();
        }
    };
    api.terminal.openTabAndWait = async (title, { timeoutMs = 30_000 } = {}) => {
        const wanted = exactPaneTitle(title);
        if (!Number.isSafeInteger(timeoutMs) || timeoutMs < 1 || timeoutMs > 30_000) {
            throw new RangeError('terminal tab wait timeout must be between 1 and 30000ms');
        }
        let changed;
        const observed = new Promise((resolve) => {
            changed = resolve;
        });
        const stop = await api.watchPaneChanges((change) => changed(change));
        let timer;
        let tab;
        try {
            tab = await api.terminal.openTab(wanted);
            const change = await Promise.race([
                observed,
                new Promise((resolve) => {
                    timer = setTimeout(() => resolve(null), timeoutMs);
                }),
            ]);
            if (change === null)
                return { changed: false, tab, title: wanted };
            const inventory = await api.terminal.panes();
            const pane = inventory.panes.find((candidate) => candidate.tab === tab);
            if (!pane)
                throw new Error(inventory.truncated
                    ? 'opened tab cannot be verified from a truncated pane inventory'
                    : 'opened tab has no observable pane');
            return { changed: true, tab, pane };
        }
        catch (cause) {
            if (tab === undefined)
                throw cause;
            throw new TerminalOperationError('open-tab', { tab, title: wanted }, cause);
        }
        finally {
            clearTimeout(timer);
            await stop();
        }
    };
    api.terminal.pinTabAndWait = async (tab, pinned = true, { timeoutMs = 30_000 } = {}) => {
        if (typeof tab !== 'string' ||
            tab.length === 0 ||
            tab.includes('\0') ||
            new TextEncoder().encode(tab).byteLength > 128) {
            throw new TypeError('terminal pin requires a 1..128 byte NUL-free tab identity');
        }
        if (typeof pinned !== 'boolean')
            throw new TypeError('terminal pin state must be boolean');
        if (!Number.isSafeInteger(timeoutMs) || timeoutMs < 1 || timeoutMs > 30_000) {
            throw new RangeError('terminal pin wait timeout must be between 1 and 30000ms');
        }
        let observed;
        let authorityIssued = false;
        const inventory = new Promise((resolve, reject) => {
            observed = (tabs) => {
                if (!authorityIssued)
                    return;
                const current = tabs.find((candidate) => candidate.id === tab);
                if (!current)
                    reject(new Error(`terminal tab ${tab} disappeared while pinning`));
                else if (current.pinned === pinned)
                    resolve(current);
            };
        });
        const stop = await api.watchTerminal(observed);
        let timer;
        try {
            authorityIssued = true;
            await api.terminal.pinTab(tab, pinned);
            const current = await Promise.race([
                inventory,
                new Promise((resolve) => {
                    timer = setTimeout(() => resolve(null), timeoutMs);
                }),
            ]);
            return current === null ? { changed: false, tab, pinned } : { changed: true, tab: current };
        }
        finally {
            clearTimeout(timer);
            await stop();
        }
    };
    api.terminal.actAndWait = async (slot, action, { lines, timeoutMs = 30_000, signal } = {}) => {
        if (typeof slot !== 'string' || slot.length === 0)
            throw new TypeError('pane semantic action requires a nonempty slot');
        exactSemanticAction(action);
        if (!Number.isSafeInteger(timeoutMs) || timeoutMs < 1 || timeoutMs > 30_000) {
            throw new RangeError('pane semantic action wait timeout must be between 1 and 30000ms');
        }
        if (signal?.aborted)
            throw outputAbort(signal);
        const scoped = signal ? api.withSignal(signal) : api;
        let changed;
        const observed = new Promise((resolve) => {
            changed = resolve;
        });
        const stop = await scoped.watchPaneChanges((change) => {
            if (change.slot === slot &&
                (change.generation !== action.generation || change.revision !== action.revision))
                changed(change);
        });
        let timer;
        let abort;
        try {
            await scoped.terminal.act(slot, action);
            const change = await Promise.race([
                observed,
                new Promise((resolve) => {
                    timer = setTimeout(() => resolve(null), timeoutMs);
                }),
                new Promise((_, reject) => {
                    abort = () => reject(outputAbort(signal));
                    signal?.addEventListener('abort', abort, { once: true });
                }),
            ]);
            if (change === null)
                return {
                    changed: false,
                    after: { generation: action.generation, revision: action.revision },
                };
            const readable = await scoped.terminal.toText(slot, { lines });
            if (readable.snapshot.generation !== action.generation) {
                throw new Error('semantic action pane was replaced before its result could be verified');
            }
            if (readable.snapshot.generation === action.generation &&
                readable.snapshot.revision === action.revision) {
                throw new Error('pane change did not advance the readable snapshot cursor');
            }
            return { changed: true, readable };
        }
        finally {
            clearTimeout(timer);
            if (abort)
                signal?.removeEventListener('abort', abort);
            await stop();
        }
    };
    api.terminal.inspectAndAct = async (slot, proposal, { timeoutMs = 30_000, signal } = {}) => {
        if (typeof slot !== 'string' || slot.length === 0)
            throw new TypeError('inspected semantic action requires a nonempty slot');
        const actions = ['invoke', 'change', 'submit', 'toggle', 'expand', 'focus'];
        if (!Number.isSafeInteger(proposal?.node) ||
            proposal.node < 0 ||
            !actions.includes(proposal?.action)) {
            throw new TypeError('inspected semantic action requires a nonnegative node and known action');
        }
        if (proposal.value != null &&
            (typeof proposal.value !== 'string' ||
                new TextEncoder().encode(proposal.value).byteLength > 4096)) {
            throw new RangeError('inspected semantic action value exceeds 4096 bytes');
        }
        if (!Number.isSafeInteger(timeoutMs) || timeoutMs < 1 || timeoutMs > 30_000) {
            throw new RangeError('inspected semantic action timeout must be between 1 and 30000ms');
        }
        if (signal?.aborted)
            throw outputAbort(signal);
        const scoped = signal ? api.withSignal(signal) : api;
        let changed;
        let cursor;
        const observed = new Promise((resolve) => {
            changed = resolve;
        });
        const stop = await scoped.watchPaneChanges((change) => {
            if (cursor &&
                change.slot === slot &&
                (change.generation !== cursor.generation || change.revision !== cursor.revision))
                changed(change);
        });
        let timer;
        let abort;
        try {
            const snapshot = await scoped.terminal.semantics(slot);
            cursor = { generation: snapshot.generation, revision: snapshot.revision };
            const pending = [snapshot.root];
            let node;
            while (pending.length > 0) {
                const candidate = pending.pop();
                if (candidate.id === proposal.node) {
                    node = candidate;
                    break;
                }
                pending.push(...candidate.children);
            }
            if (!node)
                throw new Error(snapshot.truncated
                    ? 'semantic node cannot be resolved from a truncated tree'
                    : 'semantic node does not exist');
            if (node.disabled)
                throw new Error('semantic node is disabled');
            if (!node.actions.includes(proposal.action))
                throw new Error('semantic node does not advertise the requested action');
            const before = { snapshot, ...semanticText(snapshot) };
            await scoped.terminal.act(slot, {
                ...cursor,
                node: proposal.node,
                action: proposal.action,
                value: proposal.value ?? null,
            });
            const change = await Promise.race([
                observed,
                new Promise((resolve) => {
                    timer = setTimeout(() => resolve(null), timeoutMs);
                }),
                new Promise((_, reject) => {
                    abort = () => reject(outputAbort(signal));
                    signal?.addEventListener('abort', abort, { once: true });
                }),
            ]);
            if (change === null)
                return { changed: false, before };
            const afterSnapshot = await scoped.terminal.semantics(slot);
            if (afterSnapshot.generation !== cursor.generation) {
                throw new Error('inspected semantic pane was replaced before action verification');
            }
            if (afterSnapshot.generation === cursor.generation &&
                afterSnapshot.revision === cursor.revision) {
                throw new Error('pane change did not advance the semantic tree cursor');
            }
            return {
                changed: true,
                before,
                after: { snapshot: afterSnapshot, ...semanticText(afterSnapshot) },
            };
        }
        finally {
            clearTimeout(timer);
            if (abort)
                signal?.removeEventListener('abort', abort);
            await stop();
        }
    };
    api.terminal.splitAndWait = async (slot, generation, revision, division, { timeoutMs = 30_000 } = {}) => {
        if (typeof slot !== 'string' || slot.length === 0)
            throw new TypeError('terminal split requires a nonempty slot');
        if (!Number.isSafeInteger(generation) ||
            generation < 0 ||
            !Number.isSafeInteger(revision) ||
            revision < 0) {
            throw new TypeError('terminal split requires nonnegative safe integer generation and revision');
        }
        if (division !== 'beside' && division !== 'below')
            throw new TypeError('terminal split division must be beside or below');
        if (!Number.isSafeInteger(timeoutMs) || timeoutMs < 1 || timeoutMs > 30_000) {
            throw new RangeError('terminal split wait timeout must be between 1 and 30000ms');
        }
        let changed;
        const observed = new Promise((resolve) => {
            changed = resolve;
        });
        let createdSlot;
        const stop = await api.watchPaneChanges((change) => {
            if ((change.slot === slot &&
                (change.generation !== generation || change.revision !== revision)) ||
                (createdSlot !== undefined && change.slot === createdSlot))
                changed(change);
        });
        let timer;
        try {
            createdSlot = await api.terminal.splitObserved(slot, generation, revision, division);
            const change = await Promise.race([
                observed,
                new Promise((resolve) => {
                    timer = setTimeout(() => resolve(null), timeoutMs);
                }),
            ]);
            if (change === null)
                return { changed: false, slot: createdSlot, after: { generation, revision } };
            const inventory = await api.terminal.panes();
            const pane = inventory.panes.find((candidate) => candidate.slot === createdSlot);
            if (!pane)
                throw new Error(inventory.truncated
                    ? 'created split cannot be verified from a truncated inventory'
                    : 'created split is absent from pane inventory');
            return { changed: true, pane };
        }
        finally {
            clearTimeout(timer);
            await stop();
        }
    };
    api.terminal.closeAndWait = async (slot, generation, revision, { timeoutMs = 30_000 } = {}) => {
        if (typeof slot !== 'string' || slot.length === 0)
            throw new TypeError('terminal close requires a nonempty slot');
        if (!Number.isSafeInteger(generation) ||
            generation < 0 ||
            !Number.isSafeInteger(revision) ||
            revision < 0) {
            throw new TypeError('terminal close requires nonnegative safe integer generation and revision');
        }
        if (!Number.isSafeInteger(timeoutMs) || timeoutMs < 1 || timeoutMs > 30_000) {
            throw new RangeError('terminal close wait timeout must be between 1 and 30000ms');
        }
        let pending = false;
        let wake;
        const next = () => pending
            ? Promise.resolve()
            : new Promise((resolve) => {
                wake = resolve;
            });
        const stop = await api.watchPaneChanges((change) => {
            if (change.slot !== slot ||
                (change.generation === generation && change.revision === revision))
                return;
            pending = true;
            wake?.();
            wake = undefined;
        });
        const deadline = Date.now() + timeoutMs;
        try {
            await api.terminal.closeObserved(slot, generation, revision);
            while (true) {
                const remaining = deadline - Date.now();
                if (remaining <= 0)
                    return { changed: false, slot, after: { generation, revision } };
                let timer;
                const event = await Promise.race([
                    next().then(() => true),
                    new Promise((resolve) => {
                        timer = setTimeout(() => resolve(false), remaining);
                    }),
                ]);
                clearTimeout(timer);
                if (!event)
                    return { changed: false, slot, after: { generation, revision } };
                pending = false;
                const inventory = await api.terminal.panes();
                const pane = inventory.panes.find((candidate) => candidate.slot === slot);
                if (!pane && !inventory.truncated)
                    return { changed: true, slot };
                if (pane && pane.generation !== generation) {
                    throw new Error('closed pane slot was replaced before complete absence was observed');
                }
            }
        }
        finally {
            await stop();
        }
    };
    api.terminal.retitleAndWait = async (slot, generation, revision, title, { timeoutMs = 30_000 } = {}) => {
        if (typeof slot !== 'string' || slot.length === 0)
            throw new TypeError('terminal retitle requires a nonempty slot');
        if (!Number.isSafeInteger(generation) ||
            generation < 0 ||
            !Number.isSafeInteger(revision) ||
            revision < 0) {
            throw new TypeError('terminal retitle requires nonnegative safe integer generation and revision');
        }
        const wanted = exactPaneTitle(title);
        if (!Number.isSafeInteger(timeoutMs) || timeoutMs < 1 || timeoutMs > 30_000) {
            throw new RangeError('terminal retitle wait timeout must be between 1 and 30000ms');
        }
        let changed;
        const observed = new Promise((resolve) => {
            changed = resolve;
        });
        const stop = await api.watchPaneChanges((change) => {
            if (change.slot === slot &&
                (change.generation !== generation || change.revision !== revision))
                changed(change);
        });
        let timer;
        try {
            await api.terminal.retitleObserved(slot, generation, revision, wanted);
            const change = await Promise.race([
                observed,
                new Promise((resolve) => {
                    timer = setTimeout(() => resolve(null), timeoutMs);
                }),
            ]);
            if (change === null)
                return { changed: false, title: wanted, after: { generation, revision } };
            const inventory = await api.terminal.panes();
            const pane = inventory.panes.find((candidate) => candidate.slot === slot);
            if (!pane)
                throw new Error(inventory.truncated
                    ? 'retitled pane cannot be verified from a truncated inventory'
                    : 'retitled pane disappeared');
            if (pane.generation !== generation)
                throw new Error('retitled pane slot was replaced before verification');
            if (pane.revision === revision || pane.title !== wanted) {
                throw new Error('pane changed without applying the requested title');
            }
            return { changed: true, pane };
        }
        finally {
            clearTimeout(timer);
            await stop();
        }
    };
    api.terminal.focusAndWait = async (slot, generation, revision, { timeoutMs = 30_000 } = {}) => {
        if (typeof slot !== 'string' || slot.length === 0)
            throw new TypeError('terminal focus requires a nonempty slot');
        if (!Number.isSafeInteger(generation) ||
            generation < 0 ||
            !Number.isSafeInteger(revision) ||
            revision < 0) {
            throw new TypeError('terminal focus requires nonnegative safe integer generation and revision');
        }
        if (!Number.isSafeInteger(timeoutMs) || timeoutMs < 1 || timeoutMs > 30_000) {
            throw new RangeError('terminal focus wait timeout must be between 1 and 30000ms');
        }
        let changed;
        const observed = new Promise((resolve) => {
            changed = resolve;
        });
        const stop = await api.watchPaneChanges((change) => {
            if (change.slot === slot &&
                (change.generation !== generation || change.revision !== revision))
                changed(change);
        });
        let timer;
        try {
            await api.terminal.focusObserved(slot, generation, revision);
            const change = await Promise.race([
                observed,
                new Promise((resolve) => {
                    timer = setTimeout(() => resolve(null), timeoutMs);
                }),
            ]);
            if (change === null)
                return { changed: false, slot, after: { generation, revision } };
            const inventory = await api.terminal.panes();
            const pane = inventory.panes.find((candidate) => candidate.slot === slot);
            if (!pane)
                throw new Error(inventory.truncated
                    ? 'focused pane cannot be verified from a truncated inventory'
                    : 'focused pane disappeared');
            if (pane.generation !== generation)
                throw new Error('focused pane slot was replaced before verification');
            if (pane.revision === revision || !pane.focused)
                throw new Error('pane changed without receiving focus');
            return { changed: true, pane };
        }
        finally {
            clearTimeout(timer);
            await stop();
        }
    };
    api.terminal.ratioAndWait = async (slot, generation, revision, ratio, { timeoutMs = 30_000 } = {}) => {
        if (typeof slot !== 'string' || slot.length === 0)
            throw new TypeError('terminal ratio requires a nonempty slot');
        if (!Number.isSafeInteger(generation) ||
            generation < 0 ||
            !Number.isSafeInteger(revision) ||
            revision < 0) {
            throw new TypeError('terminal ratio requires nonnegative safe integer generation and revision');
        }
        const wanted = exactPaneRatio(ratio);
        if (!Number.isSafeInteger(timeoutMs) || timeoutMs < 1 || timeoutMs > 30_000) {
            throw new RangeError('terminal ratio wait timeout must be between 1 and 30000ms');
        }
        let changed;
        const observed = new Promise((resolve) => {
            changed = resolve;
        });
        const stop = await api.watchPaneChanges((change) => {
            if (change.slot === slot &&
                (change.generation !== generation || change.revision !== revision))
                changed(change);
        });
        let timer;
        try {
            await api.terminal.ratioObserved(slot, generation, revision, wanted);
            const change = await Promise.race([
                observed,
                new Promise((resolve) => {
                    timer = setTimeout(() => resolve(null), timeoutMs);
                }),
            ]);
            if (change === null)
                return { changed: false, ratio: wanted, after: { generation, revision } };
            const inventory = await api.terminal.panes();
            const pane = inventory.panes.find((candidate) => candidate.slot === slot);
            if (!pane)
                throw new Error(inventory.truncated
                    ? 'resized split pane cannot be verified from a truncated inventory'
                    : 'resized split pane disappeared');
            if (pane.generation !== generation)
                throw new Error('resized split pane slot was replaced before verification');
            if (pane.revision === revision)
                throw new Error('pane ratio event did not advance the inspected cursor');
            const topology = await api.terminal.topology();
            const actual = topology.tabs
                .map((tab) => paneRatio(tab.root, slot))
                .find((value) => value !== null);
            if (actual == null)
                throw new Error('resized pane is not inside an observable split');
            if (Math.abs(actual - wanted) > 0.05)
                throw new Error('pane changed without applying the requested split ratio');
            return { changed: true, ratio: wanted, actual, pane };
        }
        finally {
            clearTimeout(timer);
            await stop();
        }
    };
    api.terminal.switchOccupantAndWait = async (slot, generation, revision, target, { timeoutMs = 30_000 } = {}) => {
        if (typeof slot !== 'string' || slot.length === 0)
            throw new TypeError('pane occupant switch requires a nonempty slot');
        if (!Number.isSafeInteger(generation) ||
            generation < 0 ||
            !Number.isSafeInteger(revision) ||
            revision < 0) {
            throw new TypeError('pane occupant switch requires nonnegative safe integer generation and revision');
        }
        const wanted = exactOccupantTarget(target);
        if (!Number.isSafeInteger(timeoutMs) || timeoutMs < 1 || timeoutMs > 30_000) {
            throw new RangeError('pane occupant switch wait timeout must be between 1 and 30000ms');
        }
        let changed;
        const observed = new Promise((resolve) => {
            changed = resolve;
        });
        const stop = await api.watchPaneChanges((change) => {
            if (change.slot === slot &&
                (change.generation !== generation || change.revision !== revision))
                changed(change);
        });
        let timer;
        try {
            await api.terminal.switchOccupantObserved(slot, generation, revision, wanted);
            const change = await Promise.race([
                observed,
                new Promise((resolve) => {
                    timer = setTimeout(() => resolve(null), timeoutMs);
                }),
            ]);
            if (change === null)
                return { changed: false, target: wanted, after: { generation, revision } };
            const inventory = await api.terminal.panes();
            const pane = inventory.panes.find((candidate) => candidate.slot === slot);
            if (!pane)
                throw new Error(inventory.truncated
                    ? 'switched pane cannot be verified from a truncated inventory'
                    : 'switched pane disappeared');
            const matches = wanted.kind === 'terminal'
                ? pane.kind === 'terminal'
                : pane.kind === 'surface' &&
                    pane.provider?.extension === wanted.extension &&
                    pane.provider?.provider === wanted.provider;
            if (!matches)
                throw new Error('pane changed without installing the requested occupant');
            return { changed: true, pane };
        }
        finally {
            clearTimeout(timer);
            await stop();
        }
    };
    api.extensions.waitForProviderMount = async (extension, provider, { state = 'mounted', after = null, timeoutMs = 30_000 } = {}) => {
        const providerName = (value) => typeof value === 'string' &&
            value.length <= 128 &&
            /^[A-Za-z0-9][A-Za-z0-9_.-]*$/.test(value);
        if (!providerName(extension) || !providerName(provider))
            throw new TypeError('provider wait requires exact bounded extension and provider names');
        if (!['mounted', 'unmounted'].includes(state))
            throw new TypeError('provider wait state must be mounted or unmounted');
        if (!Number.isSafeInteger(timeoutMs) || timeoutMs < 1 || timeoutMs > 30_000)
            throw new RangeError('provider wait timeout must be between 1 and 30000ms');
        if (after != null &&
            (typeof after.slot !== 'string' ||
                after.slot.length === 0 ||
                !Number.isSafeInteger(after.generation) ||
                after.generation < 0 ||
                !Number.isSafeInteger(after.revision) ||
                after.revision < 0))
            throw new TypeError('provider wait cursor requires slot, generation, and revision');
        let dispose;
        let timer;
        let settled = false;
        let checking = false;
        let pending = false;
        return new Promise((resolve, reject) => {
            const finish = (value, error) => {
                if (settled)
                    return;
                settled = true;
                clearTimeout(timer);
                Promise.resolve(dispose?.()).then(() => (error ? reject(error) : resolve(value)), reject);
            };
            const check = async () => {
                if (checking) {
                    pending = true;
                    return;
                }
                checking = true;
                try {
                    do {
                        pending = false;
                        const inventory = await api.terminal.panes();
                        const pane = inventory.panes.find((item) => item.kind === 'surface' &&
                            item.provider?.extension === extension &&
                            item.provider?.provider === provider);
                        if (!pane && inventory.truncated)
                            throw new Error('provider mount cannot be resolved from a truncated pane inventory');
                        const unchanged = pane != null &&
                            after != null &&
                            pane.slot === after.slot &&
                            pane.generation === after.generation &&
                            pane.revision === after.revision;
                        if ((state === 'mounted' && pane && !unchanged) || (state === 'unmounted' && !pane)) {
                            finish({ changed: true, state, pane: pane ?? null, truncated: false });
                            return;
                        }
                    } while (pending && !settled);
                }
                catch (error) {
                    finish(undefined, error);
                }
                finally {
                    checking = false;
                }
            };
            api
                .watchPaneChanges(() => {
                void check();
            })
                .then((stop) => {
                dispose = stop;
                if (settled)
                    void stop();
                else
                    void check();
            }, (error) => finish(undefined, error));
            timer = setTimeout(() => finish({ changed: false, state, after }), timeoutMs);
        });
    };
    api.watchExecutions = (listener) => watch('executions', 'executions', listener, 'execution');
    api.containers.signalExecutionAndWait = async (id, signal, after, { state = 'exited', timeoutMs = 30_000 } = {}) => {
        const executionId = immutableIdentity(id, [32], 'execution');
        exactExecutionSignal(signal);
        if (after == null ||
            typeof after.running !== 'boolean' ||
            !Number.isSafeInteger(after.exit_code) ||
            !Number.isSafeInteger(after.pid)) {
            throw new TypeError('execution signal wait requires the exact running, exit_code, and pid cursor');
        }
        if (state !== 'changed' && state !== 'exited')
            throw new TypeError('execution signal wait state must be changed or exited');
        if (!Number.isSafeInteger(timeoutMs) || timeoutMs < 1 || timeoutMs > 30_000) {
            throw new RangeError('execution signal wait timeout must be between 1 and 30000ms');
        }
        const differs = (execution) => execution.running !== after.running ||
            execution.exit_code !== after.exit_code ||
            execution.pid !== after.pid;
        let observed;
        const transition = new Promise((resolve, reject) => {
            observed = (catalogue) => {
                const execution = catalogue.executions.find((candidate) => candidate.id === executionId);
                if (!execution) {
                    if (!catalogue.truncated)
                        reject(new Error('execution disappeared while waiting for its signal transition'));
                    return;
                }
                if (!differs(execution) || (state === 'exited' && execution.running))
                    return;
                resolve(execution);
            };
        });
        const stop = await api.watchExecutions(observed);
        let timer;
        try {
            const current = await api.containers.execution(executionId);
            if (differs(current))
                throw new Error('execution cursor changed before signal authority');
            await api.containers.signalExecution(executionId, signal);
            const execution = await Promise.race([
                transition,
                new Promise((resolve) => {
                    timer = setTimeout(() => resolve(null), timeoutMs);
                }),
            ]);
            return execution === null
                ? { changed: false, id: executionId, state, after }
                : { changed: true, execution };
        }
        finally {
            clearTimeout(timer);
            await stop();
        }
    };
    api.containers.removeExecutionAndWait = async (id, after, { timeoutMs = 30_000 } = {}) => {
        const executionId = immutableIdentity(id, [32], 'execution');
        if (after == null ||
            typeof after.running !== 'boolean' ||
            !Number.isSafeInteger(after.exit_code) ||
            !Number.isSafeInteger(after.pid)) {
            throw new TypeError('execution removal wait requires the exact running, exit_code, and pid cursor');
        }
        if (after.running)
            throw new TypeError('execution removal wait requires an observed finished execution');
        if (!Number.isSafeInteger(timeoutMs) || timeoutMs < 1 || timeoutMs > 30_000) {
            throw new RangeError('execution removal wait timeout must be between 1 and 30000ms');
        }
        const differs = (execution) => execution.running !== after.running ||
            execution.exit_code !== after.exit_code ||
            execution.pid !== after.pid;
        let removing = false;
        let observed;
        let timer;
        const absent = new Promise((resolve) => {
            observed = (catalogue) => {
                if (!removing || catalogue.truncated)
                    return;
                if (!catalogue.executions.some((execution) => execution.id === executionId))
                    resolve();
            };
        });
        const stop = await api.watchExecutions(observed);
        try {
            const current = await api.containers.execution(executionId);
            if (differs(current))
                throw new Error('execution cursor changed before removal authority');
            removing = true;
            await api.containers.removeExecution(executionId);
            const removed = await Promise.race([
                absent.then(() => true),
                new Promise((resolve) => {
                    timer = setTimeout(() => resolve(false), timeoutMs);
                }),
            ]);
            return { changed: removed, id: executionId };
        }
        finally {
            clearTimeout(timer);
            await stop();
        }
    };
    api.watchImagePulls = (listener) => watch('image-pulls', 'image_pulls', listener, 'image pull');
    api.watchExtensions = (listener) => watch('extensions', 'extensions', listener, 'extension');
    api.extensions.enableAndWait = async (name, imageDigest, { timeoutMs = 30_000 } = {}) => {
        const digest = immutableDigest(imageDigest, 'extension image');
        if (!Number.isSafeInteger(timeoutMs) || timeoutMs < 1 || timeoutMs > 30_000) {
            throw new RangeError('extension enable wait timeout must be between 1 and 30000ms');
        }
        let observed;
        let authorityIssued = false;
        const inventory = new Promise((resolve, reject) => {
            observed = (extensions) => {
                if (!authorityIssued)
                    return;
                const current = extensions.find((extension) => extension.name === name);
                if (current && current.image_digest !== digest) {
                    reject(new Error(`extension ${name} was replaced while enabling`));
                }
                else if (current?.enabled) {
                    resolve(current);
                }
            };
        });
        const stop = await api.watchExtensions(observed);
        let timer;
        try {
            authorityIssued = true;
            await api.extensions.enable(name, digest);
            const extension = await Promise.race([
                inventory,
                new Promise((resolve) => {
                    timer = setTimeout(() => resolve(null), timeoutMs);
                }),
            ]);
            return extension === null
                ? { changed: false, name, image_digest: digest }
                : { changed: true, extension };
        }
        finally {
            clearTimeout(timer);
            await stop();
        }
    };
    api.extensions.disableAndWait = async (name, imageDigest, { timeoutMs = 30_000 } = {}) => {
        const digest = immutableDigest(imageDigest, 'extension image');
        if (!Number.isSafeInteger(timeoutMs) || timeoutMs < 1 || timeoutMs > 30_000) {
            throw new RangeError('extension disable wait timeout must be between 1 and 30000ms');
        }
        let observed;
        let authorityIssued = false;
        const inventory = new Promise((resolve, reject) => {
            observed = (extensions) => {
                if (!authorityIssued)
                    return;
                const current = extensions.find((extension) => extension.name === name);
                if (!current)
                    reject(new Error(`extension ${name} disappeared while disabling`));
                else if (current.image_digest !== digest)
                    reject(new Error(`extension ${name} was replaced while disabling`));
                else if (!current.enabled)
                    resolve(current);
            };
        });
        const stop = await api.watchExtensions(observed);
        let timer;
        try {
            authorityIssued = true;
            await api.extensions.disable(name, digest);
            const extension = await Promise.race([
                inventory,
                new Promise((resolve) => {
                    timer = setTimeout(() => resolve(null), timeoutMs);
                }),
            ]);
            return extension === null
                ? { changed: false, name, image_digest: digest }
                : { changed: true, extension };
        }
        finally {
            clearTimeout(timer);
            await stop();
        }
    };
    api.extensions.retryAndWait = async (name, imageDigest, { timeoutMs = 30_000 } = {}) => {
        const digest = immutableDigest(imageDigest, 'extension image');
        if (!Number.isSafeInteger(timeoutMs) || timeoutMs < 1 || timeoutMs > 30_000) {
            throw new RangeError('extension retry wait timeout must be between 1 and 30000ms');
        }
        let observed;
        let authorityIssued = false;
        const inventory = new Promise((resolve, reject) => {
            observed = (extensions) => {
                if (!authorityIssued)
                    return;
                const current = extensions.find((extension) => extension.name === name);
                if (!current)
                    reject(new Error(`extension ${name} disappeared while retrying`));
                else if (current.image_digest !== digest)
                    reject(new Error(`extension ${name} was replaced while retrying`));
                else if (current.enabled && current.status === 'duty')
                    resolve(current);
            };
        });
        const stop = await api.watchExtensions(observed);
        let timer;
        try {
            authorityIssued = true;
            await api.extensions.retry(name, digest);
            const extension = await Promise.race([
                inventory,
                new Promise((resolve) => {
                    timer = setTimeout(() => resolve(null), timeoutMs);
                }),
            ]);
            return extension === null
                ? { changed: false, name, image_digest: digest }
                : { changed: true, extension };
        }
        finally {
            clearTimeout(timer);
            await stop();
        }
    };
    api.extensions.removeAndWait = async (name, imageDigest, { timeoutMs = 30_000 } = {}) => {
        const digest = immutableDigest(imageDigest, 'extension image');
        if (!Number.isSafeInteger(timeoutMs) || timeoutMs < 1 || timeoutMs > 30_000) {
            throw new RangeError('extension remove wait timeout must be between 1 and 30000ms');
        }
        let observed;
        let authorityIssued = false;
        const inventory = new Promise((resolve) => {
            observed = (extensions) => {
                if (!authorityIssued)
                    return;
                const current = extensions.find((extension) => extension.name === name);
                if (!current || current.image_digest !== digest)
                    resolve(current ?? null);
            };
        });
        const stop = await api.watchExtensions(observed);
        let timer;
        try {
            authorityIssued = true;
            await api.extensions.remove(name, digest);
            const replacement = await Promise.race([
                inventory,
                new Promise((resolve) => {
                    timer = setTimeout(() => resolve(undefined), timeoutMs);
                }),
            ]);
            return replacement === undefined
                ? { changed: false, name, image_digest: digest }
                : { changed: true, removed: { name, image_digest: digest }, replacement };
        }
        finally {
            clearTimeout(timer);
            await stop();
        }
    };
    api.watchExtensionAcquisitions = (listener) => watch('extension-acquisitions', 'extension_acquisitions', listener, 'extension acquisition');
    const commitAcquisitionAndWait = async (operation, job, revision, review, { timeoutMs = 30_000 } = {}) => {
        const { capabilities: granted, containers = { selectors: [], create: false }, images = { read: [], use: [], pull: [], remove: [], prune_all_unused: false }, networks = { selectors: [], create: false }, volumes = { selectors: [], create: false }, filesystem = { read: [], write: [], create: [], delete: [], rename: [] }, workspaceEnvironment = { read: [], write: [] }, } = review;
        if (!Number.isSafeInteger(revision) || revision < 0) {
            throw new TypeError(`extension ${operation} wait requires a nonnegative safe integer revision`);
        }
        if (!Number.isSafeInteger(timeoutMs) || timeoutMs < 1 || timeoutMs > 30_000) {
            throw new RangeError(`extension ${operation} wait timeout must be between 1 and 30000ms`);
        }
        const status = await api.extensions.acquisition(job);
        if (status.job !== job ||
            status.revision !== revision ||
            status.state !== 'ready' ||
            !status.candidate) {
            throw new Error(`extension ${operation} requires the exact ready acquisition revision`);
        }
        const candidate = status.candidate;
        const digest = immutableDigest(candidate.image_digest, 'extension candidate image');
        if ((operation === 'install') !== (candidate.installed_image_digest == null)) {
            throw new Error(`extension candidate is not eligible for ${operation}`);
        }
        let observed;
        let authorityReturned = false;
        let latest;
        let committed;
        const inventory = new Promise((resolve, reject) => {
            observed = (extensions) => {
                const current = extensions.find((extension) => extension.name === candidate.name);
                if (!authorityReturned) {
                    latest = current ?? null;
                    return;
                }
                if (current?.image_digest === digest &&
                    current.version === committed?.version &&
                    extensionAuthority(current) === extensionAuthority(committed))
                    resolve(current);
                else
                    reject(new Error(`extension ${candidate.name} was replaced or disappeared after ${operation}`));
            };
        });
        const stop = await api.watchExtensions(observed);
        let timer;
        try {
            committed = await api.extensions[operation](job, revision, digest, granted, containers, images, networks, volumes, filesystem, workspaceEnvironment);
            if (committed.name !== candidate.name ||
                committed.image_digest !== digest ||
                committed.version !== candidate.version) {
                throw new Error(`extension ${operation} returned a different candidate identity`);
            }
            authorityReturned = true;
            if (latest !== undefined)
                observed(latest === null ? [] : [latest]);
            const extension = await Promise.race([
                inventory,
                new Promise((resolve) => {
                    timer = setTimeout(() => resolve(null), timeoutMs);
                }),
            ]);
            return extension === null
                ? { changed: false, name: candidate.name, image_digest: digest, revision }
                : { changed: true, extension };
        }
        finally {
            clearTimeout(timer);
            await stop();
        }
    };
    api.extensions.installAndWait = (job, revision, review, options) => commitAcquisitionAndWait('install', job, revision, review, options);
    api.extensions.updateAndWait = (job, revision, review, options) => commitAcquisitionAndWait('update', job, revision, review, options);
    api.extensions.waitForAcquisition = async (job, afterRevision, { timeoutMs = 30_000 } = {}) => {
        if (typeof job !== 'string' ||
            job.length === 0 ||
            new TextEncoder().encode(job).byteLength > 128) {
            throw new TypeError('extension acquisition wait requires a 1..128 byte job identity');
        }
        if (!Number.isSafeInteger(afterRevision) || afterRevision < 0) {
            throw new TypeError('extension acquisition wait requires a nonnegative safe integer revision');
        }
        if (!Number.isSafeInteger(timeoutMs) || timeoutMs < 1 || timeoutMs > 30_000) {
            throw new RangeError('extension acquisition wait timeout must be between 1 and 30000ms');
        }
        let dispose;
        let timer;
        let settled = false;
        let reading = false;
        let latest;
        return new Promise((resolve, reject) => {
            const finish = (value, error) => {
                if (settled)
                    return;
                settled = true;
                clearTimeout(timer);
                Promise.resolve(dispose?.()).then(() => (error ? reject(error) : resolve(value)), reject);
            };
            const refresh = async () => {
                if (settled || reading)
                    return;
                reading = true;
                try {
                    do {
                        const expectedRevision = latest?.revision;
                        latest = undefined;
                        const status = await api.extensions.acquisition(job);
                        if (status.job !== job) {
                            throw new Error('extension acquisition status returned a different job identity');
                        }
                        const requiredRevision = Math.max(expectedRevision ?? 0, latest?.revision ?? 0);
                        if (status.revision < requiredRevision) {
                            latest = { revision: requiredRevision };
                        }
                        else if (status.revision > afterRevision) {
                            latest = undefined;
                            finish({ changed: true, status });
                        }
                    } while (latest && !settled);
                }
                catch (error) {
                    finish(undefined, error);
                }
                finally {
                    reading = false;
                }
            };
            const observe = (change) => {
                if (settled || change.job !== job || change.revision <= afterRevision)
                    return;
                latest = change;
                void refresh();
            };
            api.watchExtensionAcquisitions(observe).then((stop) => {
                dispose = stop;
                if (settled)
                    void stop();
                else
                    void refresh();
            }, (error) => finish(undefined, error));
            timer = setTimeout(() => finish({ changed: false, job, revision: afterRevision }), timeoutMs);
        });
    };
    api.watchWorkspaceLifecycle = (listener) => watch('workspace-lifecycle', 'workspace_lifecycle', listener, 'workspace lifecycle');
    api.watchWorkspaceEvents = (listener) => watch('workspace-events', 'workspace_events', listener, 'workspace event');
    api.watchFilesystem = (listener) => watch('filesystem', 'filesystem', listener, 'filesystem inventory');
    return api;
}
/** Mirrors Rust Request::capability for every fixed wire call used by this public facade. */
export function requestCapability(call) {
    const capability = PROTOCOL_REQUEST_CAPABILITIES[call];
    if (capability !== undefined && capability !== null)
        return capability;
    if (capability === null)
        throw new RangeError(`extension request ${call} has topic-selected capability`);
    throw new RangeError(`unclassified extension request ${call}`);
}
const camel = (value) => value.replace(/_([a-z])/g, (_, letter) => letter.toUpperCase());
const facadeOverrides = Object.freeze({
    workspace_environment_patch: 'patchEnvironment',
    extension_acquisition_start: 'extensions.startAcquisition',
    extension_acquisition_status: 'extensions.acquisition',
    extension_acquisition_cancel: 'extensions.cancelAcquisition',
    execution_inspect: 'containers.execution',
    execution_list: 'containers.executions',
    execution_logs: 'containers.executionLogs',
    execution_output: 'containers.executionOutput',
    execution_wait: 'containers.waitExecution',
    execution_kill: 'containers.signalExecution',
    execution_cancel: 'containers.cancelExecution',
    execution_remove: 'containers.removeExecution',
    execution_write: 'containers.writeExecutionStdin',
    execution_close_input: 'containers.closeExecutionStdin',
    container_attach_terminal: 'containers.attachTerminal',
    container_exec_credential: 'containers.execWithCredentials',
    image_pull_start: 'images.startPull',
    image_pull_status: 'images.pullStatus',
    image_pull_cancel: 'images.cancelPull',
    pane_list: 'terminal.panes',
    pane_semantic_read: 'terminal.semantics',
    pane_semantic_action: 'terminal.act',
    terminal_read_pane: 'terminal.read',
    terminal_write_pane: 'terminal.writeInput',
    terminal_close_pane: 'terminal.close',
    terminal_close_pane_observed: 'terminal.closeObserved',
    terminal_focus_pane: 'terminal.focus',
    terminal_focus_pane_observed: 'terminal.focusObserved',
    terminal_retitle_pane: 'terminal.retitle',
    terminal_retitle_pane_observed: 'terminal.retitleObserved',
});
const internalRequests = Object.freeze({
    interface_open_tab: 'owned by the React/native renderer root lifecycle',
    interface_split: 'owned by the React/native renderer root lifecycle',
    interface_withdraw: 'owned by the React/native renderer root lifecycle',
    interface_render: 'owned by the React/native renderer commit transport',
    interface_render_at: 'owned by the React/native renderer commit transport',
    source_resize: 'owned by the React/native renderer virtual source transport',
    source_resize_at: 'owned by the React/native renderer virtual source transport',
});
function facadePath(call) {
    if (facadeOverrides[call])
        return facadeOverrides[call];
    for (const [prefix, group] of [
        ['workspace_', ''],
        ['extension_', 'extensions.'],
        ['container_', 'containers.'],
        ['image_', 'images.'],
        ['volume_', 'volumes.'],
        ['network_', 'networks.'],
        ['terminal_', 'terminal.'],
        ['filesystem_', 'files.'],
        ['state_', 'state.'],
        ['preference_', 'preferences.'],
        ['credential_', 'credentials.'],
        ['notification_', 'notifications.'],
    ])
        if (call.startsWith(prefix))
            return group + camel(call.slice(prefix.length));
    return null;
}
/** Schema-derived inventory connecting every Rust request/topic to its supported public route. */
export const protocolSurface = Object.freeze({
    requests: Object.freeze(Object.fromEntries(Object.keys(PROTOCOL_REPLIES).map((call) => {
        if (internalRequests[call])
            return [call, Object.freeze({ kind: 'internal', rationale: internalRequests[call] })];
        if (call === 'event_subscribe')
            return [call, Object.freeze({ kind: 'subscription', api: 'subscribe' })];
        if (call === 'event_unsubscribe')
            return [call, Object.freeze({ kind: 'subscription', api: 'unsubscribe' })];
        return [call, Object.freeze({ kind: 'facade', api: facadePath(call) })];
    }))),
    topics: Object.freeze(Object.fromEntries(PROTOCOL_TOPICS.map(({ wire }) => [
        wire,
        Object.freeze({ subscribe: 'subscribe', unsubscribe: 'unsubscribe' }),
    ]))),
});
/** Honest inventory of the current host contract; gaps are not callable APIs. */
export const protocolCoverage = Object.freeze({
    available: Object.freeze({
        workspace: [
            'info',
            'list',
            'inspect',
            'create',
            'adopt',
            'update',
            'patchEnvironment',
            'delete',
            'start',
            'stop',
            'restart',
        ],
        containers: [
            'list',
            'inspect',
            'processes',
            'logs',
            'execution',
            'executions',
            'executionLogs',
            'executionOutput',
            'executionOutputPages',
            'resumeExecutionStreaming',
            'waitExecution',
            'signalExecution',
            'cancelExecution',
            'removeExecution',
            'writeExecutionStdin',
            'closeExecutionStdin',
            'pipeExecutionStdin',
            'create',
            'start',
            'stop',
            'remove',
            'pause',
            'unpause',
            'restart',
            'rename',
            'kill',
            'exec',
            'execAndWait',
            'execStreaming',
            'execText',
            'execLines',
            'execJsonLines',
            'attachTerminal',
        ],
        images: [
            'inventory',
            'list',
            'inspect',
            'pull',
            'startPull',
            'pullStatus',
            'cancelPull',
            'remove',
            'prune',
            'removeAndWait',
        ],
        volumes: ['inventory', 'list', 'inspect', 'create', 'remove', 'removeAndWait'],
        networks: [
            'inventory',
            'list',
            'inspect',
            'create',
            'remove',
            'removeAndWait',
            'connect',
            'disconnect',
        ],
        terminal: [
            'panes',
            'tabs',
            'topology',
            'openTab',
            'pinTab',
            'split',
            'splitObserved',
            'spawn',
            'spawnObserved',
            'read',
            'semantics',
            'act',
            'writeInput',
            'resizeGrid',
            'resizeGridObserved',
            'close',
            'closeObserved',
            'focus',
            'focusObserved',
            'retitle',
            'retitleObserved',
            'ratio',
            'ratioObserved',
            'switchOccupant',
            'switchOccupantObserved',
        ],
        files: [
            'pathGrant',
            'inventory',
            'beginWalk',
            'changes',
            'catchUpChanges',
            'changePages',
            'watchChanges',
            'watchLatestChanges',
            'list',
            'listPage',
            'walk',
            'read',
            'readRange',
            'readRanges',
            'readChunks',
            'readText',
            'stat',
            'write',
            'writeObserved',
            'createObserved',
            'mkdir',
            'rename',
            'renameObserved',
            'remove',
            'removeObserved',
        ],
        state: ['read', 'write', 'clear'],
        preferences: ['read', 'set', 'remove'],
        extensions: [
            'list',
            'catalogue',
            'requireCompleteCatalogue',
            'inspect',
            'enable',
            'disable',
            'retry',
            'remove',
            'startAcquisition',
            'acquisition',
            'cancelAcquisition',
            'install',
            'update',
        ],
        notifications: ['publish'],
        interfaceEvents: [
            'invoke',
            'submit',
            'change',
            'select',
            'scroll',
            'close',
            'context',
            'key',
            'focus',
            'pointer',
            'drag',
            'drop',
        ],
        workspaceEvents: ['key', 'focus', 'pointer'],
        snapshotTopics: SNAPSHOT_TOPICS,
    }),
    unavailable: Object.freeze({
        workspace: ['renameWhileUpdating', 'mutateWhileRunning', 'controlHostingWorkspace'],
        containers: [],
        images: [],
        terminal: [],
        events: [],
        extensions: [],
    }),
});
