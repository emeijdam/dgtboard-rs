//! Transport-agnostic protocol decoder.
//!
//! [`Decoder`] buffers raw bytes from *any* source (a serial port, a Web Serial
//! reader, a test vector) and turns them into [`Event`]s. It also keeps a
//! running [`Board`] updated from board dumps and field updates. It performs no
//! I/O itself, which is what lets the same code run natively and in the browser.

use crate::board::{Board, Piece, Square};
use crate::protocol;

/// A decoded event from the board.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// A full board snapshot (reply to a board request).
    BoardDump(Board),
    /// A single square changed (sent while in update mode).
    FieldUpdate {
        square: Square,
        /// The piece now on the square, or `None` if it was lifted/emptied.
        piece: Option<Piece>,
    },
    /// Firmware version `(major, minor)`.
    Version(u8, u8),
    /// Board serial number.
    SerialNumber(String),
    /// A clock / time message, payload passed through unparsed.
    Clock(Vec<u8>),
    /// Any other message we don't model: raw id (bit stripped) + payload.
    Other { id: u8, body: Vec<u8> },
}

/// A raw, framed protocol message.
struct RawMessage {
    /// Message id with [`protocol::MESSAGE_BIT`] already stripped.
    id: u8,
    body: Vec<u8>,
}

/// Buffers bytes and yields [`Event`]s, maintaining a running board state.
#[derive(Clone, Debug, Default)]
pub struct Decoder {
    buf: Vec<u8>,
    board: Board,
    flip: bool,
}

impl Decoder {
    /// A new decoder in the board's native orientation (square 0 = a8).
    pub fn new() -> Decoder {
        Decoder::default()
    }

    /// A new decoder that reads the board rotated 180° (White at the end away
    /// from the cable).
    pub fn with_flip(flip: bool) -> Decoder {
        Decoder {
            flip,
            ..Decoder::default()
        }
    }

    /// Set whether the board is read rotated 180°. Applies to subsequent dumps
    /// and field updates.
    pub fn set_flipped(&mut self, flip: bool) {
        self.flip = flip;
    }

    /// The running board state, as maintained from dumps and field updates.
    pub fn board(&self) -> &Board {
        &self.board
    }

    /// Append received bytes to the internal buffer. Call [`poll`](Self::poll)
    /// afterwards to drain decoded events.
    pub fn push(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    /// Decode and return the next complete event, or `None` if no full message
    /// is buffered yet.
    pub fn poll(&mut self) -> Option<Event> {
        let raw = self.take_message()?;
        Some(self.decode(raw))
    }

    /// Try to pull one complete framed message out of the buffer, resyncing past
    /// any stray bytes.
    fn take_message(&mut self) -> Option<RawMessage> {
        loop {
            if self.buf.len() < 3 {
                return None;
            }
            let id = self.buf[0];
            // Resync: a valid message always starts with the message bit set.
            if id & protocol::MESSAGE_BIT == 0 {
                self.buf.remove(0);
                continue;
            }
            // Length is two 7-bit halves and includes the 3-byte header.
            let len = (((self.buf[1] & 0x7f) as usize) << 7) | (self.buf[2] & 0x7f) as usize;
            if len < 3 {
                // Implausible length: drop the header byte and resync.
                self.buf.remove(0);
                continue;
            }
            if self.buf.len() < len {
                return None;
            }
            let frame: Vec<u8> = self.buf.drain(..len).collect();
            return Some(RawMessage {
                id: id & !protocol::MESSAGE_BIT,
                body: frame[3..].to_vec(),
            });
        }
    }

    /// Turn a raw message into an [`Event`], updating the cached board state.
    fn decode(&mut self, raw: RawMessage) -> Event {
        use protocol::msg;
        match raw.id {
            msg::BOARD_DUMP => match Board::from_dump(&raw.body) {
                Ok(mut b) => {
                    if self.flip {
                        b = b.rotated_180();
                    }
                    self.board = b.clone();
                    Event::BoardDump(b)
                }
                Err(_) => Event::Other {
                    id: raw.id,
                    body: raw.body,
                },
            },
            msg::FIELD_UPDATE if raw.body.len() >= 2 && raw.body[0] < 64 => {
                let raw_idx = raw.body[0];
                let square = Square(if self.flip { 63 - raw_idx } else { raw_idx });
                let piece = Piece::from_code(raw.body[1]);
                self.board.set(square, piece);
                Event::FieldUpdate { square, piece }
            }
            msg::VERSION if raw.body.len() >= 2 => Event::Version(raw.body[0], raw.body[1]),
            msg::SERIALNR => {
                Event::SerialNumber(String::from_utf8_lossy(&raw.body).trim().to_string())
            }
            msg::BWTIME => Event::Clock(raw.body),
            _ => Event::Other {
                id: raw.id,
                body: raw.body,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Frame a message the way the board does: id|0x80, two 7-bit length bytes,
    /// then the body.
    fn frame(id: u8, body: &[u8]) -> Vec<u8> {
        let len = 3 + body.len();
        let mut v = vec![id | protocol::MESSAGE_BIT, (len >> 7) as u8, (len & 0x7f) as u8];
        v.extend_from_slice(body);
        v
    }

    #[test]
    fn decodes_a_field_update() {
        let mut d = Decoder::new();
        // square 12 (e7-ish index), white pawn (0x01)
        d.push(&frame(protocol::msg::FIELD_UPDATE, &[12, 0x01]));
        match d.poll() {
            Some(Event::FieldUpdate { square, piece }) => {
                assert_eq!(square, Square(12));
                assert_eq!(piece, Some(Piece::WhitePawn));
            }
            other => panic!("expected field update, got {other:?}"),
        }
        assert!(d.poll().is_none());
    }

    #[test]
    fn reassembles_across_split_pushes() {
        let msg = frame(protocol::msg::FIELD_UPDATE, &[0, 0x0c]);
        let mut d = Decoder::new();
        d.push(&msg[..2]); // partial header
        assert!(d.poll().is_none());
        d.push(&msg[2..]); // the rest
        assert!(matches!(d.poll(), Some(Event::FieldUpdate { .. })));
    }

    #[test]
    fn board_dump_seeds_board_and_flip_rotates() {
        let mut dump = [0u8; 64];
        dump[0] = 0x08; // black rook on a8 (native orientation)
        let mut d = Decoder::with_flip(true);
        d.push(&frame(protocol::msg::BOARD_DUMP, &dump));
        assert!(matches!(d.poll(), Some(Event::BoardDump(_))));
        // Flipped: a8 maps to h1 (index 63).
        assert_eq!(d.board().squares[63], Some(Piece::BlackRook));
        assert_eq!(d.board().squares[0], None);
    }

    #[test]
    fn resyncs_past_garbage() {
        let mut d = Decoder::new();
        d.push(&[0x00, 0x13, 0x37]); // junk without the message bit
        d.push(&frame(protocol::msg::VERSION, &[1, 6]));
        assert_eq!(d.poll(), Some(Event::Version(1, 6)));
    }
}
