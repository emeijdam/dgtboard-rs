/* tslint:disable */
/* eslint-disable */

/**
 * Fill the overlay with a canned mid-game state (see [`overlay::demo`])
 * so the marks, ghost and HUD can be eyeballed without a game.
 */
export function demo_overlay(): void;

/**
 * The bytes to write to the board to begin: reset, request a full dump,
 * then stream field updates.
 */
export function init_sequence(): Uint8Array;

/**
 * Append a chunk of bytes received from the board to the shared queue.
 */
export function push_bytes(bytes: Uint8Array): void;

/**
 * Change the board orientation live and trigger a reseed (the page should
 * re-send `init_sequence` so the position re-dumps).
 */
export function set_flip(flip: boolean): void;

/**
 * View the board from Black's side (`true`) or White's (`false`).
 */
export function set_side(black: boolean): void;

/**
 * Show the flat 2D board (`true`) or the 3D scene (`false`).
 */
export function set_view(two_d: boolean): void;

/**
 * Install a readable panic hook, store the orientation, and run the app.
 * Never returns (the winit event loop takes over).
 */
export function start(flip: boolean): void;

export type InitInput = RequestInfo | URL | Response | BufferSource | WebAssembly.Module;

export interface InitOutput {
    readonly memory: WebAssembly.Memory;
    readonly init_sequence: () => [number, number];
    readonly push_bytes: (a: number, b: number) => void;
    readonly set_flip: (a: number) => void;
    readonly set_side: (a: number) => void;
    readonly set_view: (a: number) => void;
    readonly start: (a: number) => void;
    readonly demo_overlay: () => void;
    readonly wgpu_render_bundle_draw: (a: number, b: number, c: number, d: number, e: number) => void;
    readonly wgpu_render_bundle_draw_indexed: (a: number, b: number, c: number, d: number, e: number, f: number) => void;
    readonly wgpu_render_bundle_set_pipeline: (a: number, b: bigint) => void;
    readonly wgpu_render_bundle_draw_indirect: (a: number, b: bigint, c: bigint) => void;
    readonly wgpu_render_bundle_set_bind_group: (a: number, b: number, c: bigint, d: number, e: number) => void;
    readonly wgpu_render_bundle_set_vertex_buffer: (a: number, b: number, c: bigint, d: bigint, e: bigint) => void;
    readonly wgpu_render_bundle_set_push_constants: (a: number, b: number, c: number, d: number, e: number) => void;
    readonly wgpu_render_bundle_draw_indexed_indirect: (a: number, b: bigint, c: bigint) => void;
    readonly wgpu_render_bundle_insert_debug_marker: (a: number, b: number) => void;
    readonly wgpu_render_bundle_pop_debug_group: (a: number) => void;
    readonly wgpu_render_bundle_set_index_buffer: (a: number, b: bigint, c: number, d: bigint, e: bigint) => void;
    readonly wgpu_render_bundle_push_debug_group: (a: number, b: number) => void;
    readonly wasm_bindgen_c5f4d086bd16ab8f___convert__closures_____invoke___wasm_bindgen_c5f4d086bd16ab8f___JsValue__core_37ee0ce0c4cd57d5___result__Result_____wasm_bindgen_c5f4d086bd16ab8f___JsError___true_: (a: number, b: number, c: any) => [number, number];
    readonly wasm_bindgen_c5f4d086bd16ab8f___convert__closures_____invoke___js_sys_f69620c80b62b890___Array__web_sys_b740bca31c095aca___features__gen_ResizeObserver__ResizeObserver______true_: (a: number, b: number, c: any, d: any) => void;
    readonly wasm_bindgen_c5f4d086bd16ab8f___convert__closures_____invoke___wasm_bindgen_c5f4d086bd16ab8f___JsValue______true_: (a: number, b: number, c: any) => void;
    readonly wasm_bindgen_c5f4d086bd16ab8f___convert__closures_____invoke___wasm_bindgen_c5f4d086bd16ab8f___JsValue______true__3: (a: number, b: number, c: any) => void;
    readonly wasm_bindgen_c5f4d086bd16ab8f___convert__closures_____invoke___wasm_bindgen_c5f4d086bd16ab8f___JsValue______true__4: (a: number, b: number, c: any) => void;
    readonly wasm_bindgen_c5f4d086bd16ab8f___convert__closures_____invoke___wasm_bindgen_c5f4d086bd16ab8f___JsValue______true__5: (a: number, b: number, c: any) => void;
    readonly wasm_bindgen_c5f4d086bd16ab8f___convert__closures_____invoke___wasm_bindgen_c5f4d086bd16ab8f___JsValue______true__6: (a: number, b: number, c: any) => void;
    readonly wasm_bindgen_c5f4d086bd16ab8f___convert__closures_____invoke___wasm_bindgen_c5f4d086bd16ab8f___JsValue______true__7: (a: number, b: number, c: any) => void;
    readonly wasm_bindgen_c5f4d086bd16ab8f___convert__closures_____invoke___wasm_bindgen_c5f4d086bd16ab8f___JsValue______true__8: (a: number, b: number, c: any) => void;
    readonly wasm_bindgen_c5f4d086bd16ab8f___convert__closures_____invoke___wasm_bindgen_c5f4d086bd16ab8f___JsValue______true__9: (a: number, b: number, c: any) => void;
    readonly wasm_bindgen_c5f4d086bd16ab8f___convert__closures_____invoke_______true_: (a: number, b: number) => void;
    readonly __wbindgen_malloc: (a: number, b: number) => number;
    readonly __wbindgen_realloc: (a: number, b: number, c: number, d: number) => number;
    readonly __externref_table_alloc: () => number;
    readonly __wbindgen_externrefs: WebAssembly.Table;
    readonly __wbindgen_exn_store: (a: number) => void;
    readonly __wbindgen_free: (a: number, b: number, c: number) => void;
    readonly __wbindgen_destroy_closure: (a: number, b: number) => void;
    readonly __externref_table_dealloc: (a: number) => void;
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
