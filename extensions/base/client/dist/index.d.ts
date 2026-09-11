export { ExtensionError, Session, DATA, SOCKET, PROTOCOL, validateRowRequest, validateUiEvent, } from './session.js';
export { PROTOCOL_SPECIFICATION_VERSION, PROTOCOL_VERSION, PROTOCOL_BOUNDS, PROTOCOL_CAPABILITIES, PROTOCOL_TOPICS, PROTOCOL_REPLIES, PROTOCOL_REQUEST_CAPABILITIES, encodeRequest, validateRequest, validateReply, validateReplyFor, validateFailure, validateSnapshot, } from './generated-protocol.js';
import { semanticText, semanticXml } from './semantic.js';
export { semanticText, semanticXml };
import type { CallOptions, ConnectOptions, Session as ClientSession, WorkspaceApi } from './api.js';
/** A post-creation execution failure whose immutable identity remains recoverable. */
export declare class ExecutionOperationError extends Error {
    readonly executionId: any;
    readonly phase: any;
    readonly execution: any;
    readonly after: any;
    constructor(executionId: any, phase: any, cause: any, execution?: any, after?: any);
}
/** A client-owned execution exceeded its post-start wall-clock deadline. */
export declare class ExecutionDeadlineError extends Error {
    readonly executionId: any;
    readonly deadlineMs: any;
    constructor(executionId: any, deadlineMs: any);
}
/** Output retention advanced past the cursor, so a transcript/result would be incomplete. */
export declare class ExecutionOutputGapError extends Error {
    readonly executionId: any;
    readonly after: any;
    readonly next: any;
    constructor(executionId: any, after: any, next: any);
}
/** The host returned an internally inconsistent output page, so iteration cannot continue safely. */
export declare class ExecutionOutputProtocolError extends Error {
    readonly executionId: any;
    readonly after: any;
    readonly next: any;
    constructor(executionId: any, after: any, next: any, detail: any);
}
/** One bounded structured-output record was not valid JSON. */
export declare class JsonLineParseError extends SyntaxError {
    readonly line: any;
    constructor(line: any, cause: any);
}
/** One syntactically valid JSON record did not satisfy the consumer's result schema. */
export declare class JsonLineDecodeError extends TypeError {
    readonly line: any;
    constructor(line: any, cause: any);
}
/** Durable JSON state could not be decoded; its exact identity remains available for CAS recovery. */
export declare class StateDecodeError extends TypeError {
    readonly identity: any;
    constructor(identity: any, cause: any);
}
/** Catalogue discovery was bounded before it became a complete searchable set. */
export declare class IncompleteCatalogueError extends Error {
    readonly received: any;
    constructor(received: any);
}
/** A terminal authority succeeded, but its bounded observation could not be completed. */
export declare class TerminalOperationError extends Error {
    readonly operation: any;
    readonly result: any;
    constructor(operation: any, result: any, cause: any);
}
/** A terminal text request cannot be represented by the host's bounded pane tail. */
export declare class TerminalReadLimitError extends RangeError {
    readonly requested: any;
    readonly maximum: any;
    constructor(requested: any, maximum?: number);
}
/** A pane advanced or was replaced between discovery and its bounded text projection. */
export declare class PaneChangedError extends Error {
    readonly slot: any;
    readonly expected: any;
    readonly observed: any;
    constructor(slot: any, expected: any, observed: any);
}
/** The pane layout kept changing while a bounded coherent inventory was assembled. */
export declare class PaneInventoryChangedError extends Error {
    readonly attempts: any;
    readonly before: any;
    readonly after: any;
    constructor(attempts: any, before: any, after: any);
}
/** A requested pane is absent or cannot be resolved from a bounded inventory. */
export declare class PaneUnavailableError extends Error {
    readonly slot: any;
    readonly reason: any;
    constructor(slot: any, reason: any);
}
/** Filesystem history rotated before an incremental consumer could resume its cursor. */
export declare class FilesystemJournalGapError extends Error {
    readonly requested: any;
    readonly replacement: any;
    constructor(requested: any, replacement: any);
}
/** A directory page crossed generations and enumeration must restart from its root. */
export declare class DirectoryIdentityChangedError extends Error {
    readonly path: any;
    readonly expected: any;
    readonly actual: any;
    readonly after: any;
    constructor(path: any, expected: any, actual: any, after: any);
}
/** One exact file generation could not be decoded as UTF-8. */
export declare class FileTextDecodeError extends TypeError {
    readonly path: any;
    readonly identity: any;
    readonly bytes: any;
    constructor(path: any, identity: any, bytes: any, cause: any);
}
/** One exact file generation exceeded the caller-owned text collection bound. */
export declare class FileTextLimitError extends RangeError {
    readonly path: any;
    readonly identity: any;
    readonly total: any;
    readonly limit: any;
    constructor(path: any, identity: any, total: any, limit: any);
}
/** A ranged read crossed file generations and must be restarted from a coherent identity. */
export declare class FileIdentityChangedError extends Error {
    readonly path: any;
    readonly expected: any;
    readonly actual: any;
    readonly offset: any;
    constructor(path: any, expected: any, actual: any, offset: any);
}
/** One identity reported contradictory file extents across ranged reads. */
export declare class FileExtentChangedError extends Error {
    readonly path: any;
    readonly identity: any;
    readonly expectedTotal: any;
    readonly actualTotal: any;
    readonly offset: any;
    constructor(path: any, identity: any, expectedTotal: any, actualTotal: any, offset: any);
}
export declare function connect(options?: ConnectOptions): Promise<ClientSession>;
/**
 * Opens a surface and paints a dependency-free first frame.
 *
 * Extensions can do this before importing React or another renderer, keeping
 * cold-start feedback independent of framework initialization. Pass the
 * returned token to the renderer so it continues the same frame sequence.
 */
export declare function bootstrapSurface(session: any, { title, label, primary }?: {
    title?: string;
    label?: string;
    primary?: boolean;
}): Promise<Readonly<{
    slot: string;
    sequence: 1;
    nextNode: 2;
    bootstrapNode: 1;
}>>;
export declare function workspace(session: ClientSession, { signal }?: CallOptions): WorkspaceApi;
/** Mirrors Rust Request::capability for every fixed wire call used by this public facade. */
export declare function requestCapability(call: any): any;
/** Schema-derived inventory connecting every Rust request/topic to its supported public route. */
export declare const protocolSurface: Readonly<{
    requests: any;
    topics: Readonly<{
        [k: string]: Readonly<{
            subscribe: "subscribe";
            unsubscribe: "unsubscribe";
        }>;
    }>;
}>;
/** Honest inventory of the current host contract; gaps are not callable APIs. */
export declare const protocolCoverage: Readonly<{
    available: Readonly<{
        workspace: string[];
        containers: string[];
        images: string[];
        volumes: string[];
        networks: string[];
        terminal: string[];
        files: string[];
        state: string[];
        preferences: string[];
        extensions: string[];
        notifications: string[];
        interfaceEvents: string[];
        workspaceEvents: string[];
        snapshotTopics: readonly string[];
    }>;
    unavailable: Readonly<{
        workspace: string[];
        containers: any[];
        images: any[];
        terminal: any[];
        events: any[];
        extensions: any[];
    }>;
}>;
