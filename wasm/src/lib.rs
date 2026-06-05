//! WebAssembly bindings for `dgtboard-core`.
//!
//! Pair this with the browser's Web Serial API: open the board's serial port in
//! JavaScript, write [`init_sequence`]'s bytes to start it, then pipe every
//! chunk the board sends into [`DgtSession::push`]. Read the current position
//! with [`DgtSession::fen`] and pull detected moves with
//! [`DgtSession::take_moves`]. All the real work happens in the shared,
//! I/O-free core — this file is just the glue.

use dgtboard_core::protocol::cmd;
use dgtboard_core::{Decoder, Event, MoveTracker};
use wasm_bindgen::prelude::*;

/// A live decoding session: a [`Decoder`] plus a [`MoveTracker`] seeded from the
/// first board dump.
#[wasm_bindgen]
pub struct DgtSession {
    decoder: Decoder,
    tracker: Option<MoveTracker>,
    moves: Vec<String>,
}

#[wasm_bindgen]
impl DgtSession {
    /// Create a session. Pass `flip = true` if White sits at the end of the
    /// board away from the cable.
    #[wasm_bindgen(constructor)]
    pub fn new(flip: bool) -> DgtSession {
        DgtSession {
            decoder: Decoder::with_flip(flip),
            tracker: None,
            moves: Vec::new(),
        }
    }

    /// Feed raw bytes received from the board. Drains every complete message,
    /// updating the board state and recording any detected moves.
    pub fn push(&mut self, bytes: &[u8]) {
        self.decoder.push(bytes);
        while let Some(event) = self.decoder.poll() {
            match event {
                Event::BoardDump(board) => {
                    // (Re)seed the move tracker from a full position.
                    self.tracker = Some(MoveTracker::new(board));
                }
                Event::FieldUpdate { .. } => {
                    if let Some(tracker) = self.tracker.as_mut() {
                        if let Some(mv) = tracker.update(self.decoder.board()) {
                            self.moves
                                .push(format!("{}\t{}\t{}", mv.color, mv.uci(), mv.describe()));
                        }
                    }
                }
                _ => {}
            }
        }
    }

    /// The current position as a FEN placement string.
    pub fn fen(&self) -> String {
        self.decoder.board().fen_placement()
    }

    /// The current position as an ASCII diagram.
    pub fn ascii(&self) -> String {
        self.decoder.board().ascii()
    }

    /// Whose turn it is (`"White"`, `"Black"`, or `""` if unknown).
    #[wasm_bindgen(js_name = sideToMove)]
    pub fn side_to_move(&self) -> String {
        match self.tracker.as_ref().and_then(|t| t.side_to_move()) {
            Some(color) => color.to_string(),
            None => String::new(),
        }
    }

    /// Drain moves detected since the last call. Returns a newline-separated
    /// list; each line is `color\tuci\tdescription`.
    #[wasm_bindgen(js_name = takeMoves)]
    pub fn take_moves(&mut self) -> String {
        let out = self.moves.join("\n");
        self.moves.clear();
        out
    }
}

/// The bytes to send to the board to begin: reset to idle, request a full
/// dump (seeds the position), then enter update mode (streams field changes).
#[wasm_bindgen(js_name = initSequence)]
pub fn init_sequence() -> Vec<u8> {
    vec![cmd::SEND_RESET, cmd::SEND_BRD, cmd::SEND_UPDATE_BRD]
}

/// The library version, for display.
#[wasm_bindgen]
pub fn version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}
