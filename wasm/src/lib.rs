//! WebAssembly bindings for `dgtboard-core`, with rules-aware refereeing.
//!
//! Pair this with the browser's Web Serial API: open the board's serial port in
//! JavaScript, write [`init_sequence`]'s bytes to start it, then pipe every
//! chunk the board sends into [`DgtSession::push`]. Read the position with
//! [`DgtSession::fen`], pull refereed events with [`DgtSession::take_events`],
//! and highlight check via [`DgtSession::checked_square`]. All the real work
//! happens in the shared core — this file is just glue.

use dgtboard_core::protocol::cmd;
use dgtboard_core::{Decoder, Event, RefereedGame, Ruling, Status};
use wasm_bindgen::prelude::*;

/// A live decoding + refereeing session.
#[wasm_bindgen]
pub struct DgtSession {
    decoder: Decoder,
    game: RefereedGame,
    events: Vec<String>,
    ply: u32,
}

#[wasm_bindgen]
impl DgtSession {
    /// Create a session. Pass `flip = true` if White sits at the end of the
    /// board away from the cable. Refereeing assumes the game starts from the
    /// standard initial position.
    #[wasm_bindgen(constructor)]
    pub fn new(flip: bool) -> DgtSession {
        DgtSession {
            decoder: Decoder::with_flip(flip),
            game: RefereedGame::new(),
            events: Vec::new(),
            ply: 0,
        }
    }

    /// Feed raw bytes from the board. Drains every complete message, updates the
    /// board, referees each move, and records events.
    pub fn push(&mut self, bytes: &[u8]) {
        self.decoder.push(bytes);
        while let Some(event) = self.decoder.poll() {
            match event {
                Event::BoardDump(_) => {
                    // A full dump means a fresh game from the start position.
                    self.game = RefereedGame::new();
                    self.ply = 0;
                }
                Event::FieldUpdate { .. } => match self.game.update(self.decoder.board()) {
                    Some(Ruling::Legal { san, status, .. }) => {
                        self.ply += 1;
                        let mover = if self.ply % 2 == 1 { "White" } else { "Black" };
                        self.events.push(format!(
                            "move\t{}\t{}\t{}\t{}",
                            self.ply,
                            mover,
                            san,
                            status_word(status)
                        ));
                    }
                    Some(Ruling::Illegal { uci }) => {
                        self.events.push(format!("illegal\t{uci}"));
                    }
                    Some(Ruling::BackInSync) => self.events.push("sync".to_string()),
                    None => {}
                },
                _ => {}
            }
        }
    }

    /// The current position as a FEN placement string.
    pub fn fen(&self) -> String {
        self.decoder.board().fen_placement()
    }

    /// The current game status as a word: `normal`, `check`, `checkmate:White`,
    /// `checkmate:Black`, `stalemate`, or `draw`.
    pub fn status(&self) -> String {
        status_word(self.game.status())
    }

    /// The square of the king in check, in DGT index order (0 = a8), or `-1`.
    #[wasm_bindgen(js_name = checkedSquare)]
    pub fn checked_square(&self) -> i32 {
        self.game.checked_square().map(|i| i as i32).unwrap_or(-1)
    }

    /// Drain events recorded since the last call, newline-separated. Each line
    /// is one of:
    /// - `move\t<ply>\t<color>\t<san>\t<status>`
    /// - `illegal\t<uci>`
    /// - `sync`
    #[wasm_bindgen(js_name = takeEvents)]
    pub fn take_events(&mut self) -> String {
        let out = self.events.join("\n");
        self.events.clear();
        out
    }
}

fn status_word(status: Status) -> String {
    match status {
        Status::Normal => "normal".to_string(),
        Status::Check => "check".to_string(),
        Status::Checkmate { winner } => format!("checkmate:{winner}"),
        Status::Stalemate => "stalemate".to_string(),
        Status::DrawInsufficientMaterial => "draw".to_string(),
    }
}

/// The bytes to send to the board to begin: reset to idle, request a full dump
/// (seeds the position), then enter update mode (streams field changes).
#[wasm_bindgen(js_name = initSequence)]
pub fn init_sequence() -> Vec<u8> {
    vec![cmd::SEND_RESET, cmd::SEND_BRD, cmd::SEND_UPDATE_BRD]
}

/// The library version, for display.
#[wasm_bindgen]
pub fn version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}
