//! Move detection: reconstruct chess moves from the board's field-update stream.
//!
//! A DGT board reports individual square changes (a piece lifted here, a piece
//! placed there), not whole moves. A single move can span several such updates
//! — and during a capture or castle the board passes through intermediate
//! states with pieces "in hand". [`MoveTracker`] reconstructs logical moves by
//! diffing the live physical board against the last *settled* position and only
//! emitting a move once that diff forms one complete, consistent pattern.
//!
//! This is deliberately a geometric reconstruction, not a legal-move engine: it
//! trusts that the moves played on the board are legal and figures out *which*
//! move happened. It does not validate legality, detect check/mate, or generate
//! SAN.
//!
//! Feed it the running board after each field update (from a [`Decoder`], a
//! native `DgtBoard`, or a browser Web Serial reader):
//!
//! ```
//! use dgtboard_core::{Decoder, Event, MoveTracker};
//!
//! let mut decoder = Decoder::new();
//! let mut tracker: Option<MoveTracker> = None;
//!
//! // `bytes` are whatever the board sent over the wire.
//! # let bytes: Vec<u8> = vec![];
//! decoder.push(&bytes);
//! while let Some(event) = decoder.poll() {
//!     match event {
//!         Event::BoardDump(board) => tracker = Some(MoveTracker::new(board)),
//!         Event::FieldUpdate { .. } => {
//!             if let Some(t) = tracker.as_mut() {
//!                 if let Some(mv) = t.update(decoder.board()) {
//!                     println!("{}  {}", mv.uci(), mv.describe());
//!                 }
//!             }
//!         }
//!         _ => {}
//!     }
//! }
//! ```
//!
//! [`Decoder`]: crate::Decoder

use crate::board::{Board, Color, Piece, Square};

/// A set of `(square, piece)` entries — the unit the board diff works in.
type SquarePieces = Vec<(Square, Piece)>;

/// Which side a castling move is on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CastleSide {
    Kingside,
    Queenside,
}

/// The nature of a detected move.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MoveKind {
    /// A non-capturing move.
    Quiet,
    /// A capture; carries the captured piece.
    Capture(Piece),
    /// An en passant capture (the captured pawn sits off the destination square).
    EnPassant,
    /// Castling.
    Castle(CastleSide),
    /// A pawn promotion.
    Promotion {
        /// The piece the pawn became (as physically placed on the board).
        to: Piece,
        /// A piece captured in the same move, if the promotion was a capture.
        captured: Option<Piece>,
    },
}

/// A reconstructed move.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct DetectedMove {
    /// Origin square (the king's square, for castling).
    pub from: Square,
    /// Destination square (the king's square, for castling).
    pub to: Square,
    /// The piece that moved (the king, for castling; the pawn, for promotion).
    pub piece: Piece,
    /// The side that moved.
    pub color: Color,
    /// What kind of move it was.
    pub kind: MoveKind,
}

impl DetectedMove {
    /// Long algebraic / UCI notation, e.g. `e2e4`, `e1g1` (castling), `e7e8q`
    /// (promotion).
    pub fn uci(&self) -> String {
        let mut s = format!("{}{}", self.from, self.to);
        if let MoveKind::Promotion { to, .. } = self.kind {
            s.push(to.fen_char().to_ascii_lowercase());
        }
        s
    }

    /// A readable, glyph-decorated description, e.g. `♗ c4xf7`, `O-O`,
    /// `♙ e7-e8=♕`.
    pub fn describe(&self) -> String {
        match self.kind {
            MoveKind::Castle(CastleSide::Kingside) => "O-O".to_string(),
            MoveKind::Castle(CastleSide::Queenside) => "O-O-O".to_string(),
            MoveKind::Quiet => format!("{} {}-{}", self.piece.glyph(), self.from, self.to),
            MoveKind::Capture(c) => {
                format!("{} {}x{} ({})", self.piece.glyph(), self.from, self.to, c.glyph())
            }
            MoveKind::EnPassant => {
                format!("{} {}x{} e.p.", self.piece.glyph(), self.from, self.to)
            }
            MoveKind::Promotion { to, captured } => {
                let sep = if captured.is_some() { 'x' } else { '-' };
                let mut s = format!(
                    "{} {}{}{}={}",
                    self.piece.glyph(),
                    self.from,
                    sep,
                    self.to,
                    to.glyph()
                );
                if let Some(c) = captured {
                    s.push_str(&format!(" ({})", c.glyph()));
                }
                s
            }
        }
    }
}

/// Reconstructs moves from successive physical board states.
pub struct MoveTracker {
    confirmed: Board,
    side_to_move: Option<Color>,
}

impl MoveTracker {
    /// Start tracking from a known position. If it is the standard starting
    /// position, side-to-move is initialised to White; otherwise it is unknown
    /// until the first move reveals it.
    pub fn new(start: Board) -> MoveTracker {
        let side = if start == Board::startpos() {
            Some(Color::White)
        } else {
            None
        };
        MoveTracker {
            confirmed: start,
            side_to_move: side,
        }
    }

    /// Start tracking from a known position with an explicit side to move.
    pub fn with_side(start: Board, side: Color) -> MoveTracker {
        MoveTracker {
            confirmed: start,
            side_to_move: Some(side),
        }
    }

    /// The last settled position.
    pub fn confirmed(&self) -> &Board {
        &self.confirmed
    }

    /// Whose turn it is, if known.
    pub fn side_to_move(&self) -> Option<Color> {
        self.side_to_move
    }

    /// Feed the latest physical board state. Returns a move iff the board has
    /// just settled into a position exactly one move ahead of the confirmed
    /// one; otherwise `None` (no change, or a move still in progress).
    pub fn update(&mut self, physical: &Board) -> Option<DetectedMove> {
        let (lifted, placed) = self.diff(physical);
        let mv = self.classify(&lifted, &placed)?;
        self.apply(&mv);
        self.side_to_move = Some(mv.color.opposite());
        Some(mv)
    }

    /// Squares that lost a piece (`lifted`) and squares now holding a piece that
    /// differs from the confirmed position (`placed`, includes captures).
    fn diff(&self, physical: &Board) -> (SquarePieces, SquarePieces) {
        let mut lifted = Vec::new();
        let mut placed = Vec::new();
        for i in 0..64 {
            let sq = Square(i as u8);
            match (self.confirmed.squares[i], physical.squares[i]) {
                (Some(was), None) => lifted.push((sq, was)),
                (None, Some(now)) => placed.push((sq, now)),
                (Some(was), Some(now)) if was != now => placed.push((sq, now)),
                _ => {}
            }
        }
        (lifted, placed)
    }

    fn classify(
        &self,
        lifted: &[(Square, Piece)],
        placed: &[(Square, Piece)],
    ) -> Option<DetectedMove> {
        match (lifted.len(), placed.len()) {
            (0, 0) => None,
            (1, 1) => self.classify_simple(lifted[0], placed[0]),
            (2, 1) => self.classify_en_passant(lifted, placed[0]),
            (2, 2) => detect_castle(lifted, placed),
            _ => None, // incomplete or contradictory: wait for more updates
        }
    }

    /// One piece lifted, one square changed: quiet move, capture, or promotion.
    /// Castling-in-progress sub-states are deferred (return `None`).
    fn classify_simple(
        &self,
        (from, moved): (Square, Piece),
        (to, now): (Square, Piece),
    ) -> Option<DetectedMove> {
        let color = moved.color();
        let captured = self.confirmed.get(to); // enemy piece previously on `to`, if any

        // King moving two squares == castling; wait for the rook too.
        if moved.is_king() && from.rank() == to.rank() && from.file().abs_diff(to.file()) == 2 {
            return None;
        }
        // Rook from its castling home to the castled square while its king is
        // still home: likely castling-in-progress, defer.
        if moved.is_rook() && self.is_castle_rook_move(from, to, color) && self.king_home(color) {
            return None;
        }

        // Promotion: a pawn reaching the back rank (the placed piece is whatever
        // was physically set down — the board identifies it).
        if moved.is_pawn() && to.is_promotion_rank(color) {
            return Some(DetectedMove {
                from,
                to,
                piece: moved,
                color,
                kind: MoveKind::Promotion {
                    to: now,
                    captured: captured.filter(|c| c.color() != color),
                },
            });
        }

        // Otherwise the piece identity must be preserved.
        if now != moved {
            return None;
        }
        let kind = match captured {
            Some(c) if c.color() != color => MoveKind::Capture(c),
            Some(_) => return None, // "captured" own piece: inconsistent
            None => MoveKind::Quiet,
        };
        Some(DetectedMove {
            from,
            to,
            piece: moved,
            color,
            kind,
        })
    }

    /// Two pieces lifted, one placed on a previously-empty square: en passant.
    fn classify_en_passant(
        &self,
        lifted: &[(Square, Piece)],
        (to, now): (Square, Piece),
    ) -> Option<DetectedMove> {
        if !now.is_pawn() || self.confirmed.get(to).is_some() {
            return None;
        }
        let color = now.color();
        // The mover came from an adjacent file on the rank behind `to`.
        let mover = lifted
            .iter()
            .find(|&&(s, p)| p == now && s.file().abs_diff(to.file()) == 1)?;
        // The captured pawn is an enemy pawn on the mover's rank, on `to`'s file.
        let captured_sq = Square::at(to.file(), mover.0.rank());
        let captured_ok = lifted.iter().any(|&(s, p)| {
            s == captured_sq && p.is_pawn() && p.color() == color.opposite()
        });
        if !captured_ok {
            return None;
        }
        Some(DetectedMove {
            from: mover.0,
            to,
            piece: now,
            color,
            kind: MoveKind::EnPassant,
        })
    }

    fn is_castle_rook_move(&self, from: Square, to: Square, color: Color) -> bool {
        CASTLES
            .iter()
            .any(|c| c.color == color && c.rook_from == from && c.rook_to == to)
    }

    fn king_home(&self, color: Color) -> bool {
        let (sq, king) = match color {
            Color::White => (Square::at(4, 1), Piece::WhiteKing),
            Color::Black => (Square::at(4, 8), Piece::BlackKing),
        };
        self.confirmed.get(sq) == Some(king)
    }

    fn apply(&mut self, mv: &DetectedMove) {
        match mv.kind {
            MoveKind::Quiet | MoveKind::Capture(_) => {
                self.confirmed.set(mv.from, None);
                self.confirmed.set(mv.to, Some(mv.piece));
            }
            MoveKind::Promotion { to, .. } => {
                self.confirmed.set(mv.from, None);
                self.confirmed.set(mv.to, Some(to));
            }
            MoveKind::EnPassant => {
                let captured_sq = Square::at(mv.to.file(), mv.from.rank());
                self.confirmed.set(mv.from, None);
                self.confirmed.set(captured_sq, None);
                self.confirmed.set(mv.to, Some(mv.piece));
            }
            MoveKind::Castle(side) => {
                let c = CASTLES
                    .iter()
                    .find(|c| c.color == mv.color && c.side == side)
                    .expect("castle spec exists");
                let (king, rook) = match mv.color {
                    Color::White => (Piece::WhiteKing, Piece::WhiteRook),
                    Color::Black => (Piece::BlackKing, Piece::BlackRook),
                };
                self.confirmed.set(c.king_from, None);
                self.confirmed.set(c.rook_from, None);
                self.confirmed.set(c.king_to, Some(king));
                self.confirmed.set(c.rook_to, Some(rook));
            }
        }
    }
}

/// A single castling specification.
struct CastleSpec {
    color: Color,
    side: CastleSide,
    king_from: Square,
    king_to: Square,
    rook_from: Square,
    rook_to: Square,
}

/// The four castling moves, by exact square.
const CASTLES: [CastleSpec; 4] = [
    CastleSpec {
        color: Color::White,
        side: CastleSide::Kingside,
        king_from: Square::at(4, 1), // e1
        king_to: Square::at(6, 1),   // g1
        rook_from: Square::at(7, 1), // h1
        rook_to: Square::at(5, 1),   // f1
    },
    CastleSpec {
        color: Color::White,
        side: CastleSide::Queenside,
        king_from: Square::at(4, 1), // e1
        king_to: Square::at(2, 1),   // c1
        rook_from: Square::at(0, 1), // a1
        rook_to: Square::at(3, 1),   // d1
    },
    CastleSpec {
        color: Color::Black,
        side: CastleSide::Kingside,
        king_from: Square::at(4, 8), // e8
        king_to: Square::at(6, 8),   // g8
        rook_from: Square::at(7, 8), // h8
        rook_to: Square::at(5, 8),   // f8
    },
    CastleSpec {
        color: Color::Black,
        side: CastleSide::Queenside,
        king_from: Square::at(4, 8), // e8
        king_to: Square::at(2, 8),   // c8
        rook_from: Square::at(0, 8), // a8
        rook_to: Square::at(3, 8),   // d8
    },
];

fn detect_castle(
    lifted: &[(Square, Piece)],
    placed: &[(Square, Piece)],
) -> Option<DetectedMove> {
    let has = |set: &[(Square, Piece)], sq: Square, p: Piece| set.iter().any(|&(s, q)| s == sq && q == p);
    for c in &CASTLES {
        let (king, rook) = match c.color {
            Color::White => (Piece::WhiteKing, Piece::WhiteRook),
            Color::Black => (Piece::BlackKing, Piece::BlackRook),
        };
        if has(lifted, c.king_from, king)
            && has(lifted, c.rook_from, rook)
            && has(placed, c.king_to, king)
            && has(placed, c.rook_to, rook)
        {
            return Some(DetectedMove {
                from: c.king_from,
                to: c.king_to,
                piece: king,
                color: c.color,
                kind: MoveKind::Castle(c.side),
            });
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Apply a sequence of physical board states to the tracker, collecting the
    /// moves it emits. Each state in `frames` is the full physical board after
    /// one field update.
    fn run(start: Board, frames: &[Board]) -> Vec<DetectedMove> {
        let mut t = MoveTracker::new(start);
        frames.iter().filter_map(|f| t.update(f)).collect()
    }

    /// Move a piece on a clone of `b`, leaving `from` empty.
    fn moved(b: &Board, steps: &[(Square, Option<Piece>)]) -> Board {
        let mut c = b.clone();
        for &(sq, p) in steps {
            c.set(sq, p);
        }
        c
    }

    #[test]
    fn quiet_pawn_move_two_updates() {
        let start = Board::startpos();
        let e2 = Square::at(4, 2);
        let e4 = Square::at(4, 4);
        // lift e2, then place on e4
        let f1 = moved(&start, &[(e2, None)]);
        let f2 = moved(&f1, &[(e4, Some(Piece::WhitePawn))]);
        let moves = run(start, &[f1, f2]);
        assert_eq!(moves.len(), 1);
        assert_eq!(moves[0].uci(), "e2e4");
        assert_eq!(moves[0].kind, MoveKind::Quiet);
    }

    #[test]
    fn capture_lifting_victim_first() {
        // White bishop on c4 takes black pawn on f7 (start-like custom board).
        let mut start = Board::empty();
        let c4 = Square::at(2, 4);
        let f7 = Square::at(5, 7);
        start.set(c4, Some(Piece::WhiteBishop));
        start.set(f7, Some(Piece::BlackPawn));
        let mut t = MoveTracker::with_side(start.clone(), Color::White);

        let f1 = moved(&start, &[(f7, None)]); // lift victim
        let f2 = moved(&f1, &[(c4, None)]); // lift bishop
        let f3 = moved(&f2, &[(f7, Some(Piece::WhiteBishop))]); // place bishop

        assert!(t.update(&f1).is_none());
        assert!(t.update(&f2).is_none());
        let mv = t.update(&f3).expect("capture detected");
        assert_eq!(mv.uci(), "c4f7");
        assert_eq!(mv.kind, MoveKind::Capture(Piece::BlackPawn));
    }

    #[test]
    fn kingside_castle_king_first() {
        let mut start = Board::empty();
        let e1 = Square::at(4, 1);
        let h1 = Square::at(7, 1);
        let g1 = Square::at(6, 1);
        let f1 = Square::at(5, 1);
        start.set(e1, Some(Piece::WhiteKing));
        start.set(h1, Some(Piece::WhiteRook));
        let mut t = MoveTracker::with_side(start.clone(), Color::White);

        let s1 = moved(&start, &[(e1, None)]); // lift king
        let s2 = moved(&s1, &[(g1, Some(Piece::WhiteKing))]); // king to g1 (deferred)
        let s3 = moved(&s2, &[(h1, None)]); // lift rook
        let s4 = moved(&s3, &[(f1, Some(Piece::WhiteRook))]); // rook to f1

        assert!(t.update(&s1).is_none());
        assert!(t.update(&s2).is_none(), "king two-square must defer");
        assert!(t.update(&s3).is_none());
        let mv = t.update(&s4).expect("castle detected");
        assert_eq!(mv.kind, MoveKind::Castle(CastleSide::Kingside));
        assert_eq!(mv.uci(), "e1g1");
    }

    #[test]
    fn white_en_passant() {
        // White pawn e5 captures black pawn d5 en passant, landing on d6.
        let mut start = Board::empty();
        let e5 = Square::at(4, 5);
        let d5 = Square::at(3, 5);
        let d6 = Square::at(3, 6);
        start.set(e5, Some(Piece::WhitePawn));
        start.set(d5, Some(Piece::BlackPawn));
        let mut t = MoveTracker::with_side(start.clone(), Color::White);

        let f1 = moved(&start, &[(e5, None)]); // lift mover
        let f2 = moved(&f1, &[(d5, None)]); // lift captured pawn
        let f3 = moved(&f2, &[(d6, Some(Piece::WhitePawn))]); // place on d6

        assert!(t.update(&f1).is_none());
        assert!(t.update(&f2).is_none());
        let mv = t.update(&f3).expect("en passant detected");
        assert_eq!(mv.kind, MoveKind::EnPassant);
        assert_eq!(mv.uci(), "e5d6");
        // captured pawn square is cleared
        assert_eq!(t.confirmed().get(d5), None);
    }

    #[test]
    fn promotion_to_queen() {
        let mut start = Board::empty();
        let e7 = Square::at(4, 7);
        let e8 = Square::at(4, 8);
        start.set(e7, Some(Piece::WhitePawn));
        let mut t = MoveTracker::with_side(start.clone(), Color::White);

        let f1 = moved(&start, &[(e7, None)]);
        let f2 = moved(&f1, &[(e8, Some(Piece::WhiteQueen))]);

        assert!(t.update(&f1).is_none());
        let mv = t.update(&f2).expect("promotion detected");
        assert_eq!(mv.uci(), "e7e8q");
        assert!(matches!(
            mv.kind,
            MoveKind::Promotion {
                to: Piece::WhiteQueen,
                captured: None
            }
        ));
    }

    #[test]
    fn adjusting_a_piece_is_not_a_move() {
        let start = Board::startpos();
        let e2 = Square::at(4, 2);
        let f1 = moved(&start, &[(e2, None)]); // lift
        let f2 = start.clone(); // put back
        let moves = run(start, &[f1, f2]);
        assert!(moves.is_empty());
    }
}
