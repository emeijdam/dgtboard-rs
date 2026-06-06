//! Rules-aware refereeing: validate detected moves against the real laws of
//! chess and report game status (check / checkmate / stalemate / draw).
//!
//! [`MoveTracker`](crate::MoveTracker) figures out *what* was physically played;
//! this layer (built on [`shakmaty`]) decides whether that move is *legal* and
//! what it means for the game. Enable the `rules` feature to use it.
//!
//! A [`RefereedGame`] keeps a real chess position in lock-step with the board.
//! When the pieces settle into a legal move it reports [`Ruling::Legal`] with
//! SAN and [`Status`]; when they settle into something illegal it reports
//! [`Ruling::Illegal`] and remembers that the board has drifted out of sync,
//! until the player restores a legal position ([`Ruling::BackInSync`]).

use shakmaty::{Chess, Color as SColor, Position, Role, Square as SSquare};

use crate::board::{Board, Color, Piece};
use crate::game::MoveTracker;

/// The game state after a move.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Status {
    /// Normal position, nothing special.
    Normal,
    /// The side to move is in check.
    Check,
    /// Checkmate — the game is over.
    Checkmate {
        /// The side that delivered mate.
        winner: Color,
    },
    /// Stalemate — a draw.
    Stalemate,
    /// A draw by insufficient material.
    DrawInsufficientMaterial,
}

impl Status {
    /// Whether this status ends the game.
    pub fn is_game_over(self) -> bool {
        !matches!(self, Status::Normal | Status::Check)
    }
}

/// Why an attempted move isn't legal — coarse, beginner-friendly categories.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum IllegalReason {
    /// The moved piece belongs to the side that isn't to move.
    NotYourTurn,
    /// There was no piece on the from-square in the legal game (board drifted).
    NoPieceThere,
    /// The mover's king is in check and the move doesn't address it.
    MustGetOutOfCheck,
    /// The destination already holds one of the mover's own pieces.
    OwnPieceOnTarget,
    /// The piece can't make that move (wrong geometry, or it exposes the king).
    IllegalMove,
}

/// The verdict on a settled board change.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ruling {
    /// A legal move was played.
    Legal {
        /// Long algebraic / UCI (e.g. `e2e4`, `e1g1`, `e7e8q`).
        uci: String,
        /// Standard algebraic notation, with check/mate suffix (e.g. `Qxh7#`).
        san: String,
        /// Resulting game status.
        status: Status,
    },
    /// A move was played that isn't legal in the current position. The board is
    /// now out of sync with the refereed game until it's restored.
    Illegal {
        /// The attempted move in UCI.
        uci: String,
        /// Why it isn't legal.
        reason: IllegalReason,
        /// The piece that was moved (for naming it in a message).
        piece: Piece,
    },
    /// The board was out of sync and has been restored to the legal position.
    BackInSync,
}

/// A chess game refereed against the physical board.
pub struct RefereedGame {
    tracker: MoveTracker,
    position: Chess,
    in_sync: bool,
}

impl Default for RefereedGame {
    fn default() -> Self {
        RefereedGame::new()
    }
}

impl RefereedGame {
    /// Start a refereed game from the standard initial position.
    pub fn new() -> RefereedGame {
        RefereedGame {
            tracker: MoveTracker::new(Board::startpos()),
            position: Chess::default(),
            in_sync: true,
        }
    }

    /// The current game status.
    pub fn status(&self) -> Status {
        status_of(&self.position)
    }

    /// Whether the physical board currently matches the legal position.
    pub fn in_sync(&self) -> bool {
        self.in_sync
    }

    /// Whose turn it is in the refereed game.
    pub fn turn(&self) -> Color {
        turn_color(&self.position)
    }

    /// The square (in DGT index order, 0 = a8) of the king that is in check, if
    /// any — useful for highlighting.
    pub fn checked_square(&self) -> Option<usize> {
        if self.position.is_check() {
            let king = self.position.board().king_of(self.position.turn())?;
            Some(our_index(king))
        } else {
            None
        }
    }

    /// Feed the latest physical board. Returns a [`Ruling`] when something
    /// decisive happens (a move completes, or the board re-syncs), else `None`.
    pub fn update(&mut self, physical: &Board) -> Option<Ruling> {
        if !self.in_sync {
            // Out of sync: only thing we care about is the board being restored.
            if physical.squares == legal_board(&self.position).squares {
                self.in_sync = true;
                self.tracker =
                    MoveTracker::with_side(legal_board(&self.position), turn_color(&self.position));
                return Some(Ruling::BackInSync);
            }
            return None;
        }

        let mv = self.tracker.update(physical)?;
        let uci = mv.uci();

        let legal = self
            .position
            .legal_moves()
            .into_iter()
            .find(|m| m.to_uci(shakmaty::CastlingMode::Standard).to_string() == uci);

        match legal {
            Some(m) => {
                let san = shakmaty::san::SanPlus::from_move(self.position.clone(), m).to_string();
                self.position = self
                    .position
                    .clone()
                    .play(m)
                    .expect("move was just verified legal");
                let status = status_of(&self.position);
                Some(Ruling::Legal { uci, san, status })
            }
            None => {
                self.in_sync = false;
                let reason = self.illegal_reason(&mv);
                Some(Ruling::Illegal {
                    uci,
                    reason,
                    piece: mv.piece,
                })
            }
        }
    }

    /// Classify *why* a detected move is illegal, for friendly explanations.
    fn illegal_reason(&self, mv: &crate::DetectedMove) -> IllegalReason {
        let from = shak_square(mv.from.0 as usize);
        let to = shak_square(mv.to.0 as usize);
        let board = self.position.board();
        match board.piece_at(from) {
            None => IllegalReason::NoPieceThere,
            Some(p) if p.color != self.position.turn() => IllegalReason::NotYourTurn,
            Some(p) => {
                if self.position.is_check() {
                    IllegalReason::MustGetOutOfCheck
                } else if board.piece_at(to).map(|t| t.color) == Some(p.color) {
                    IllegalReason::OwnPieceOnTarget
                } else {
                    IllegalReason::IllegalMove
                }
            }
        }
    }
}

fn status_of(pos: &Chess) -> Status {
    if pos.is_checkmate() {
        Status::Checkmate {
            winner: turn_color(pos).opposite(),
        }
    } else if pos.is_stalemate() {
        Status::Stalemate
    } else if pos.is_insufficient_material() {
        Status::DrawInsufficientMaterial
    } else if pos.is_check() {
        Status::Check
    } else {
        Status::Normal
    }
}

fn turn_color(pos: &Chess) -> Color {
    match pos.turn() {
        SColor::White => Color::White,
        SColor::Black => Color::Black,
    }
}

/// shakmaty square (a1 = 0) → our index (a8 = 0). Self-inverse.
fn our_index(sq: SSquare) -> usize {
    let s = sq as usize;
    (s % 8) + 8 * (7 - s / 8)
}

/// our index (a8 = 0) → shakmaty square (a1 = 0).
fn shak_square(idx: usize) -> SSquare {
    SSquare::new(((idx % 8) + 8 * (7 - idx / 8)) as u32)
}

fn to_our_piece(p: shakmaty::Piece) -> Piece {
    use Piece::*;
    match (p.color, p.role) {
        (SColor::White, Role::Pawn) => WhitePawn,
        (SColor::White, Role::Knight) => WhiteKnight,
        (SColor::White, Role::Bishop) => WhiteBishop,
        (SColor::White, Role::Rook) => WhiteRook,
        (SColor::White, Role::Queen) => WhiteQueen,
        (SColor::White, Role::King) => WhiteKing,
        (SColor::Black, Role::Pawn) => BlackPawn,
        (SColor::Black, Role::Knight) => BlackKnight,
        (SColor::Black, Role::Bishop) => BlackBishop,
        (SColor::Black, Role::Rook) => BlackRook,
        (SColor::Black, Role::Queen) => BlackQueen,
        (SColor::Black, Role::King) => BlackKing,
    }
}

/// The placement of a shakmaty position as our [`Board`].
fn legal_board(pos: &Chess) -> Board {
    let mut board = Board::empty();
    let sboard = pos.board();
    for idx in 0..64 {
        if let Some(p) = sboard.piece_at(shak_square(idx)) {
            board.squares[idx] = Some(to_our_piece(p));
        }
    }
    board
}

#[cfg(test)]
mod tests {
    use super::*;
    use shakmaty::{CastlingMode, fen::Fen};

    fn position(fen: &str) -> Chess {
        fen.parse::<Fen>()
            .unwrap()
            .into_position(CastlingMode::Standard)
            .unwrap()
    }

    #[test]
    fn detects_checkmate_and_winner() {
        // Fool's mate: White is checkmated, Black wins.
        let pos = position("rnb1kbnr/pppp1ppp/8/4p3/6Pq/5P2/PPPPP2P/RNBQKBNR w KQkq - 1 3");
        assert_eq!(
            status_of(&pos),
            Status::Checkmate {
                winner: Color::Black
            }
        );
    }

    #[test]
    fn detects_stalemate() {
        let pos = position("7k/5Q2/6K1/8/8/8/8/8 b - - 0 1");
        assert_eq!(status_of(&pos), Status::Stalemate);
    }

    #[test]
    fn detects_check() {
        let pos = position("rnbqkbnr/ppp2ppp/8/3pp3/6PQ/8/PPPPPP1P/RNB1KBNR b KQkq - 0 1");
        // Bishop on b5 checks the black king along b5–e8; the king can escape,
        // so it's check, not mate.
        let checked = position("rnbqkbnr/ppp2ppp/8/1B1pp3/4P3/8/PPPP1PPP/RNBQK1NR b KQkq - 0 1");
        assert_eq!(status_of(&pos), Status::Normal);
        assert_eq!(status_of(&checked), Status::Check);
    }

    #[test]
    fn detects_insufficient_material() {
        let pos = position("8/8/4k3/8/8/4K3/8/8 w - - 0 1"); // K vs K
        assert_eq!(status_of(&pos), Status::DrawInsufficientMaterial);
    }

    #[test]
    fn referees_legal_illegal_and_resync() {
        use crate::board::{Board, Piece, Square};
        let mut game = RefereedGame::new();
        let mut b = Board::startpos();
        let (e2, e4, e6) = (Square::at(4, 2), Square::at(4, 4), Square::at(4, 6));

        // 1. e4 — lift then place; the move completes on the second frame.
        b.set(e2, None);
        assert!(game.update(&b).is_none());
        b.set(e4, Some(Piece::WhitePawn));
        assert_eq!(
            game.update(&b),
            Some(Ruling::Legal {
                uci: "e2e4".into(),
                san: "e4".into(),
                status: Status::Normal
            })
        );

        // Illegal: slide that same pawn two more squares, e4 -> e6.
        b.set(e4, None);
        assert!(game.update(&b).is_none());
        b.set(e6, Some(Piece::WhitePawn));
        // It's Black's turn after 1.e4, so moving the White pawn again is
        // flagged as "not your turn".
        assert_eq!(
            game.update(&b),
            Some(Ruling::Illegal {
                uci: "e4e6".into(),
                reason: IllegalReason::NotYourTurn,
                piece: Piece::WhitePawn,
            })
        );
        assert!(!game.in_sync());

        // While out of sync, nothing is reported until the board is restored.
        b.set(e6, None);
        assert!(game.update(&b).is_none());
        b.set(e4, Some(Piece::WhitePawn));
        assert_eq!(game.update(&b), Some(Ruling::BackInSync));
        assert!(game.in_sync());
    }

    #[test]
    fn illegal_reason_not_your_turn() {
        use crate::board::{Board, Piece, Square};
        let mut game = RefereedGame::new();
        let mut b = Board::startpos();
        let (e2, e4, d2, d4) = (
            Square::at(4, 2),
            Square::at(4, 4),
            Square::at(3, 2),
            Square::at(3, 4),
        );
        // 1. e4 (legal)
        b.set(e2, None);
        game.update(&b);
        b.set(e4, Some(Piece::WhitePawn));
        assert!(matches!(game.update(&b), Some(Ruling::Legal { .. })));
        // White tries to move again — not White's turn.
        b.set(d2, None);
        game.update(&b);
        b.set(d4, Some(Piece::WhitePawn));
        assert_eq!(
            game.update(&b),
            Some(Ruling::Illegal {
                uci: "d2d4".into(),
                reason: IllegalReason::NotYourTurn,
                piece: Piece::WhitePawn,
            })
        );
    }

    #[test]
    fn square_mapping_is_self_inverse() {
        for idx in 0..64 {
            assert_eq!(our_index(shak_square(idx)), idx);
        }
        // a8 (our 0) ↔ shakmaty A8 (index 56)
        assert_eq!(shak_square(0) as usize, 56);
        // h1 (our 63) ↔ shakmaty H1 (index 7)
        assert_eq!(shak_square(63) as usize, 7);
    }
}
