//! `dgtboard-core` — the DGT electronic chess board protocol, with **no I/O**.
//!
//! This crate is pure logic: byte framing, board/piece decoding, and move
//! detection. It has no dependencies and no transport, so it runs anywhere —
//! a native serial app, a server, or a browser via WebAssembly. Feed it bytes
//! with [`Decoder::push`] and pull out [`Event`]s with [`Decoder::poll`]; build
//! whole moves from the event stream with [`MoveTracker`].
//!
//! ```
//! use dgtboard_core::{Decoder, Event};
//!
//! let mut decoder = Decoder::new();
//! decoder.push(&serial_bytes_from_somewhere());
//! while let Some(event) = decoder.poll() {
//!     match event {
//!         Event::BoardDump(board) => println!("{}", board.fen_placement()),
//!         Event::FieldUpdate { square, piece } => println!("{square}: {piece:?}"),
//!         _ => {}
//!     }
//! }
//! # fn serial_bytes_from_somewhere() -> Vec<u8> { vec![] }
//! ```
//!
//! For the native serial transport and CLI, see the `dgtboard` crate.

use std::fmt;

pub mod board;
pub mod decoder;
pub mod game;
pub mod protocol;
#[cfg(feature = "rules")]
pub mod rules;

pub use board::{Board, Color, Piece, Square};
pub use decoder::{Decoder, Event};
pub use game::{CastleSide, DetectedMove, MoveKind, MoveTracker};
#[cfg(feature = "rules")]
pub use rules::{RefereedGame, Ruling, Status};

/// Errors produced while decoding the protocol.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// A message was malformed (e.g. a board dump of the wrong length).
    Protocol(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Protocol(m) => write!(f, "malformed message: {m}"),
        }
    }
}

impl std::error::Error for Error {}

/// Convenience alias.
pub type Result<T> = std::result::Result<T, Error>;
