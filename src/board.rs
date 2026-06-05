//! Board representation: pieces, squares, and a parsed 64-square board.

use std::fmt;

/// A chess piece as reported by the board.
///
/// The DGT protocol also defines codes `0x0d..=0x0f` for the three "result"
/// markers (draw / white-win / black-win pieces) and several draughts pieces;
/// those are intentionally not modelled here and decode to `None`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Piece {
    WhitePawn,
    WhiteRook,
    WhiteKnight,
    WhiteBishop,
    WhiteKing,
    WhiteQueen,
    BlackPawn,
    BlackRook,
    BlackKnight,
    BlackBishop,
    BlackKing,
    BlackQueen,
}

impl Piece {
    /// Decode a raw DGT piece byte. Returns `None` for empty (`0x00`) and any
    /// non-chess marker code.
    pub fn from_code(code: u8) -> Option<Piece> {
        use Piece::*;
        Some(match code {
            0x01 => WhitePawn,
            0x02 => WhiteRook,
            0x03 => WhiteKnight,
            0x04 => WhiteBishop,
            0x05 => WhiteKing,
            0x06 => WhiteQueen,
            0x07 => BlackPawn,
            0x08 => BlackRook,
            0x09 => BlackKnight,
            0x0a => BlackBishop,
            0x0b => BlackKing,
            0x0c => BlackQueen,
            _ => return None,
        })
    }

    /// The FEN letter for this piece (uppercase = white, lowercase = black).
    pub fn fen_char(self) -> char {
        use Piece::*;
        match self {
            WhitePawn => 'P',
            WhiteRook => 'R',
            WhiteKnight => 'N',
            WhiteBishop => 'B',
            WhiteKing => 'K',
            WhiteQueen => 'Q',
            BlackPawn => 'p',
            BlackRook => 'r',
            BlackKnight => 'n',
            BlackBishop => 'b',
            BlackKing => 'k',
            BlackQueen => 'q',
        }
    }

    /// A Unicode chess glyph, for pretty terminal output.
    pub fn glyph(self) -> char {
        use Piece::*;
        match self {
            WhiteKing => '♔',
            WhiteQueen => '♕',
            WhiteRook => '♖',
            WhiteBishop => '♗',
            WhiteKnight => '♘',
            WhitePawn => '♙',
            BlackKing => '♚',
            BlackQueen => '♛',
            BlackRook => '♜',
            BlackBishop => '♝',
            BlackKnight => '♞',
            BlackPawn => '♟',
        }
    }
}

/// A board square, indexed exactly as the DGT protocol numbers fields:
/// `0` = a8, increasing along the rank, `63` = h1.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Square(pub u8);

impl Square {
    /// File index, `0` = a .. `7` = h.
    pub fn file(self) -> u8 {
        self.0 % 8
    }

    /// Human rank number, `1`..=`8`.
    pub fn rank(self) -> u8 {
        8 - self.0 / 8
    }
}

impl fmt::Display for Square {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}{}", (b'a' + self.file()) as char, self.rank())
    }
}

/// A full 64-square board snapshot.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Board {
    /// Squares in DGT order: index 0 = a8 .. index 63 = h1.
    pub squares: [Option<Piece>; 64],
}

impl Board {
    /// An empty board.
    pub fn empty() -> Board {
        Board {
            squares: [None; 64],
        }
    }

    /// Build a board from a 64-byte `BOARD_DUMP` payload.
    pub fn from_dump(dump: &[u8]) -> Result<Board, crate::Error> {
        if dump.len() != 64 {
            return Err(crate::Error::Protocol(format!(
                "board dump must be 64 bytes, got {}",
                dump.len()
            )));
        }
        let mut squares = [None; 64];
        for (i, &code) in dump.iter().enumerate() {
            squares[i] = Piece::from_code(code);
        }
        Ok(Board { squares })
    }

    /// The board rotated 180° (square `i` ↔ `63 - i`).
    ///
    /// The hardware numbers squares from the physical corner by the cable and
    /// can't tell which side White is on, so a board set up "the other way
    /// round" reads as this rotation of the intended position.
    pub fn rotated_180(&self) -> Board {
        let mut squares = [None; 64];
        for (i, slot) in squares.iter_mut().enumerate() {
            *slot = self.squares[63 - i];
        }
        Board { squares }
    }

    /// Piece on a square.
    pub fn get(&self, sq: Square) -> Option<Piece> {
        self.squares[sq.0 as usize]
    }

    /// Place (or clear, with `None`) a piece on a square.
    pub fn set(&mut self, sq: Square, piece: Option<Piece>) {
        self.squares[sq.0 as usize] = piece;
    }

    /// The piece-placement field of a FEN string (just the board, no side /
    /// castling / move counters — the board hardware can't know those).
    ///
    /// DGT's square ordering (a8..h1, rank 8 first) is already FEN order, so
    /// this is a direct walk over `squares`.
    pub fn fen_placement(&self) -> String {
        let mut fen = String::with_capacity(72);
        for rank in 0..8 {
            let mut empty = 0u8;
            for file in 0..8 {
                match self.squares[rank * 8 + file] {
                    Some(p) => {
                        if empty > 0 {
                            fen.push((b'0' + empty) as char);
                            empty = 0;
                        }
                        fen.push(p.fen_char());
                    }
                    None => empty += 1,
                }
            }
            if empty > 0 {
                fen.push((b'0' + empty) as char);
            }
            if rank < 7 {
                fen.push('/');
            }
        }
        fen
    }

    /// A human-readable board, rank 8 at the top.
    pub fn ascii(&self) -> String {
        let mut out = String::new();
        for rank in 0..8 {
            out.push((b'8' - rank as u8) as char);
            out.push(' ');
            for file in 0..8 {
                let c = match self.squares[rank * 8 + file] {
                    Some(p) => p.glyph(),
                    None => '·',
                };
                out.push(c);
                out.push(' ');
            }
            out.push('\n');
        }
        out.push_str("  a b c d e f g h\n");
        out
    }
}

impl fmt::Display for Board {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.ascii())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn square_naming() {
        assert_eq!(Square(0).to_string(), "a8");
        assert_eq!(Square(7).to_string(), "h8");
        assert_eq!(Square(56).to_string(), "a1");
        assert_eq!(Square(63).to_string(), "h1");
    }

    #[test]
    fn standard_start_position_fen() {
        // Build the standard array using the DGT dump byte codes.
        // Rank 8 (a8..h8): r n b q k b n r
        // Rank 7: 8 black pawns; rank 2: 8 white pawns; rank 1: white pieces.
        let mut dump = [0u8; 64];
        let back_black = [0x08, 0x09, 0x0a, 0x0c, 0x0b, 0x0a, 0x09, 0x08];
        let back_white = [0x02, 0x03, 0x04, 0x06, 0x05, 0x04, 0x03, 0x02];
        dump[0..8].copy_from_slice(&back_black);
        for i in 8..16 {
            dump[i] = 0x07; // black pawns
        }
        for i in 48..56 {
            dump[i] = 0x01; // white pawns
        }
        dump[56..64].copy_from_slice(&back_white);

        let board = Board::from_dump(&dump).unwrap();
        assert_eq!(
            board.fen_placement(),
            "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR"
        );
    }

    #[test]
    fn empty_board_fen() {
        assert_eq!(Board::empty().fen_placement(), "8/8/8/8/8/8/8/8");
    }
}
