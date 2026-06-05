/* tslint:disable */
/* eslint-disable */

/**
 * A live decoding session: a [`Decoder`] plus a [`MoveTracker`] seeded from the
 * first board dump.
 */
export class DgtSession {
    free(): void;
    [Symbol.dispose](): void;
    /**
     * The current position as an ASCII diagram.
     */
    ascii(): string;
    /**
     * The current position as a FEN placement string.
     */
    fen(): string;
    /**
     * Create a session. Pass `flip = true` if White sits at the end of the
     * board away from the cable.
     */
    constructor(flip: boolean);
    /**
     * Feed raw bytes received from the board. Drains every complete message,
     * updating the board state and recording any detected moves.
     */
    push(bytes: Uint8Array): void;
    /**
     * Whose turn it is (`"White"`, `"Black"`, or `""` if unknown).
     */
    sideToMove(): string;
    /**
     * Drain moves detected since the last call. Returns a newline-separated
     * list; each line is `color\tuci\tdescription`.
     */
    takeMoves(): string;
}

/**
 * The bytes to send to the board to begin: reset to idle, request a full
 * dump (seeds the position), then enter update mode (streams field changes).
 */
export function initSequence(): Uint8Array;

/**
 * The library version, for display.
 */
export function version(): string;

export type InitInput = RequestInfo | URL | Response | BufferSource | WebAssembly.Module;

export interface InitOutput {
    readonly memory: WebAssembly.Memory;
    readonly __wbg_dgtsession_free: (a: number, b: number) => void;
    readonly dgtsession_ascii: (a: number) => [number, number];
    readonly dgtsession_fen: (a: number) => [number, number];
    readonly dgtsession_new: (a: number) => number;
    readonly dgtsession_push: (a: number, b: number, c: number) => void;
    readonly dgtsession_sideToMove: (a: number) => [number, number];
    readonly dgtsession_takeMoves: (a: number) => [number, number];
    readonly initSequence: () => [number, number];
    readonly version: () => [number, number];
    readonly __wbindgen_externrefs: WebAssembly.Table;
    readonly __wbindgen_free: (a: number, b: number, c: number) => void;
    readonly __wbindgen_malloc: (a: number, b: number) => number;
    readonly __wbindgen_start: () => void;
}

export type SyncInitInput = BufferSource | WebAssembly.Module;

/**
 * Instantiates the given `module`, which can either be bytes or
 * a precompiled `WebAssembly.Module`.
 *
 * @param {{ module: SyncInitInput }} module - Passing `SyncInitInput` directly is deprecated.
 *
 * @returns {InitOutput}
 */
export function initSync(module: { module: SyncInitInput } | SyncInitInput): InitOutput;

/**
 * If `module_or_path` is {RequestInfo} or {URL}, makes a request and
 * for everything else, calls `WebAssembly.instantiate` directly.
 *
 * @param {{ module_or_path: InitInput | Promise<InitInput> }} module_or_path - Passing `InitInput` directly is deprecated.
 *
 * @returns {Promise<InitOutput>}
 */
export default function __wbg_init (module_or_path?: { module_or_path: InitInput | Promise<InitInput> } | InitInput | Promise<InitInput>): Promise<InitOutput>;
