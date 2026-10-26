/* tslint:disable */
/* eslint-disable */

/**
 * A live decoding + refereeing session.
 */
export class DgtSession {
    free(): void;
    [Symbol.dispose](): void;
    /**
     * The square of the king in check, in DGT index order (0 = a8), or `-1`.
     */
    checkedSquare(): number;
    /**
     * The current position as a FEN placement string.
     */
    fen(): string;
    /**
     * Whether the physical board matches the legal game (false right after an
     * illegal move, until the position is restored).
     */
    inSync(): boolean;
    /**
     * Whether the board is currently the standard starting position — referee
     * mode needs this to begin. Useful for warning when the board is set up
     * wrong or the flip is the wrong way round.
     */
    isStartPosition(): boolean;
    /**
     * Create a session. Pass `flip = true` if White sits at the end of the
     * board away from the cable. Refereeing assumes the game starts from the
     * standard initial position.
     */
    constructor(flip: boolean);
    /**
     * Feed raw bytes from the board. Drains every complete message, updates the
     * board, referees each move, and records events.
     */
    push(bytes: Uint8Array): void;
    /**
     * Whose turn it is in the refereed game (`"White"` / `"Black"`).
     */
    sideToMove(): string;
    /**
     * The current game status as a word: `normal`, `check`, `checkmate:White`,
     * `checkmate:Black`, `stalemate`, or `draw`.
     */
    status(): string;
    /**
     * Drain events recorded since the last call, newline-separated. Each line
     * is one of:
     * - `move\t<ply>\t<color>\t<san>\t<status>\t<uci>`
     * - `illegal\t<uci>\t<reason>\t<piece>` (reason: turn/nopiece/check/own/move)
     * - `sync`
     */
    takeEvents(): string;
}

/**
 * The bytes to send to the board to begin: reset to idle, request a full dump
 * (seeds the position), then enter update mode (streams field changes).
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
    readonly dgtsession_checkedSquare: (a: number) => number;
    readonly dgtsession_fen: (a: number) => [number, number];
    readonly dgtsession_inSync: (a: number) => number;
    readonly dgtsession_isStartPosition: (a: number) => number;
    readonly dgtsession_new: (a: number) => number;
    readonly dgtsession_push: (a: number, b: number, c: number) => void;
    readonly dgtsession_sideToMove: (a: number) => [number, number];
    readonly dgtsession_status: (a: number) => [number, number];
    readonly dgtsession_takeEvents: (a: number) => [number, number];
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
