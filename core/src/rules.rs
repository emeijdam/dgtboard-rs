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
//!
//! A game can start from any position ([`RefereedGame::from_fen`]) and can be
//! told which move to *expect* next ([`RefereedGame::expect_move`]) — for a
//! puzzle trainer or an online opponent whose reply the player has to execute
//! on a board that has no lights: the game then reports
//! [`Ruling::ExpectedPlayed`] once that move is on the board, or
//! [`Ruling::WrongMove`] if something else was played instead.

use std::fmt;

use shakmaty::{uci::UciMove, CastlingMode, Chess, Color as SColor, Position, Role, Square as SSquare};

use crate::board::{Board, Color, Piece, Square};
use crate::game::MoveTracker;

/// A position or move could not be interpreted (bad FEN, illegal UCI move).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RulesError(pub String);

impl fmt::Display for RulesError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for RulesError {}

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
    /// The move announced with [`RefereedGame::expect_move`] is now on the
    /// board; the game continues from the position after it.
    ExpectedPlayed {
        /// The expected move, in UCI as it was given to `expect_move`.
        uci: String,
        /// The expected move in SAN, with check/mate suffix.
        san: String,
        /// Game status after the move.
        status: Status,
    },
    /// A move was expected but the board settled into a different one. The
    /// game stays in the expecting state until the expected move is on the
    /// board (so the player has to take the wrong move back first).
    WrongMove {
        /// The move that was expected (UCI).
        expected: String,
        /// The move that was actually played (UCI).
        got: String,
    },
}

/// A move the game is waiting to see on the board.
#[derive(Clone, Debug)]
struct Pending {
    uci: String,
    san: String,
    /// The board before the expected move, with the mover, so a wrong move
    /// taken back registers as an adjustment rather than another move.
    before: Board,
    mover: Color,
}

/// A chess game refereed against the physical board.
#[derive(Clone)]
pub struct RefereedGame {
    tracker: MoveTracker,
    position: Chess,
    in_sync: bool,
    pending: Option<Pending>,
}

impl Default for RefereedGame {
    fn default() -> Self {
        RefereedGame::new()
    }
}

impl RefereedGame {
    /// Start a refereed game from the standard initial position.
    pub fn new() -> RefereedGame {
        RefereedGame::from_position(Chess::default())
    }

    /// Start a refereed game from a FEN (full FEN: placement, side to move,
    /// castling rights, en-passant square; the move counters may be omitted).
    /// The game assumes the physical board already shows this position — call
    /// [`with_board`](Self::with_board) to check that against a real board.
    pub fn from_fen(fen: &str) -> Result<RefereedGame, RulesError> {
        let position: Chess = fen
            .parse::<shakmaty::fen::Fen>()
            .map_err(|e| RulesError(format!("bad FEN {fen:?}: {e}")))?
            .into_position(CastlingMode::Standard)
            .map_err(|e| RulesError(format!("illegal position {fen:?}: {e}")))?;
        Ok(RefereedGame::from_position(position))
    }

    /// Start a refereed game from a shakmaty position.
    pub fn from_position(position: Chess) -> RefereedGame {
        RefereedGame {
            tracker: MoveTracker::with_side(legal_board(&position), turn_color(&position)),
            position,
            in_sync: true,
            pending: None,
        }
    }

    /// Compare the game with the physical board: the game is in sync iff
    /// `physical` already shows the expected position; otherwise it is out of
    /// sync until the pieces are set up ([`Ruling::BackInSync`]).
    pub fn with_board(mut self, physical: &Board) -> RefereedGame {
        self.in_sync = physical.squares == self.expected_board().squares;
        if self.in_sync {
            self.tracker = MoveTracker::with_side(self.expected_board(), self.turn());
        }
        self
    }

    /// The refereed position (after the expected move, while one is pending).
    pub fn position(&self) -> &Chess {
        &self.position
    }

    /// The placement the physical board should show right now.
    pub fn expected_board(&self) -> Board {
        legal_board(&self.position)
    }

    /// Squares where `physical` differs from [`expected_board`](Self::expected_board):
    /// `(square, expected piece, actual piece)`, in DGT square order.
    pub fn diff(&self, physical: &Board) -> Vec<(Square, Option<Piece>, Option<Piece>)> {
        let expected = self.expected_board();
        (0..64u8)
            .map(Square)
            .filter_map(|sq| {
                let (want, have) = (expected.get(sq), physical.get(sq));
                (want != have).then_some((sq, want, have))
            })
            .collect()
    }

    /// The SAN of a UCI move in the current position, or `None` if it isn't
    /// legal here. Doesn't change the game.
    pub fn san(&self, uci: &str) -> Option<String> {
        let m = UciMove::from_ascii(uci.as_bytes()).ok()?.to_move(&self.position).ok()?;
        Some(shakmaty::san::SanPlus::from_move(self.position.clone(), m).to_string())
    }

    /// The move the game is currently waiting for, in UCI, if any.
    pub fn expected_move(&self) -> Option<&str> {
        self.pending.as_ref().map(|p| p.uci.as_str())
    }

    /// Announce the next move (UCI; castling as `e1g1` or king-takes-rook
    /// `e1h1`, promotion as `e7e8q`) and wait for it to appear on the board.
    /// The move is applied to the position at once; the physical board is out
    /// of sync until [`update`](Self::update) reports [`Ruling::ExpectedPlayed`].
    /// Returns the move in SAN. Fails if the move is not legal here.
    pub fn expect_move(&mut self, uci: &str) -> Result<String, RulesError> {
        if self.pending.is_some() {
            return Err(RulesError(format!(
                "already waiting for {} to be played",
                self.expected_move().unwrap_or_default()
            )));
        }
        let m = UciMove::from_ascii(uci.as_bytes())
            .map_err(|e| RulesError(format!("bad UCI move {uci:?}: {e}")))?
            .to_move(&self.position)
            .map_err(|_| RulesError(format!("{uci} is not legal in this position")))?;
        let before = self.expected_board();
        let mover = self.turn();
        let san = shakmaty::san::SanPlus::from_move(self.position.clone(), m).to_string();
        self.position = self
            .position
            .clone()
            .play(m)
            .expect("move was just converted from a legal UCI move");
        self.pending = Some(Pending {
            uci: uci.to_string(),
            san: san.clone(),
            before,
            mover,
        });
        self.in_sync = false;
        Ok(san)
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
        if let Some(pending) = &self.pending {
            if physical.squares == self.expected_board().squares {
                let Pending { uci, san, .. } = self.pending.take().expect("pending checked above");
                self.in_sync = true;
                self.tracker = MoveTracker::with_side(self.expected_board(), self.turn());
                let status = self.status();
                return Some(Ruling::ExpectedPlayed { uci, san, status });
            }
            // Any other settled move is the wrong one. Re-arm the tracker on the
            // pre-move board so taking it back reads as an adjustment, not a move.
            let got = self.tracker.update(physical)?.uci();
            let expected = pending.uci.clone();
            self.tracker = MoveTracker::with_side(pending.before.clone(), pending.mover);
            return Some(Ruling::WrongMove { expected, got });
        }

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

pub(crate) fn status_of(pos: &Chess) -> Status {
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

pub(crate) fn turn_color(pos: &Chess) -> Color {
    match pos.turn() {
        SColor::White => Color::White,
        SColor::Black => Color::Black,
    }
}

/// shakmaty square (a1 = 0) → our index (a8 = 0). Self-inverse.
pub(crate) fn our_index(sq: SSquare) -> usize {
    let s = sq as usize;
    (s % 8) + 8 * (7 - s / 8)
}

/// our index (a8 = 0) → shakmaty square (a1 = 0).
pub(crate) fn shak_square(idx: usize) -> SSquare {
    SSquare::new(((idx % 8) + 8 * (7 - idx / 8)) as u32)
}

pub(crate) fn to_our_piece(p: shakmaty::Piece) -> Piece {
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
pub(crate) fn legal_board(pos: &Chess) -> Board {
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

    /// Play `uci` on `b` the way a human does: lift, then place (captures lift
    /// the victim first). Each intermediate board is fed to `game`; the rulings
    /// are returned in order.
    fn play(game: &mut RefereedGame, b: &mut Board, uci: &str) -> Vec<Ruling> {
        use crate::board::Piece::*;
        let sq = |s: &str| {
            let f = s.as_bytes()[0] - b'a';
            let r = s.as_bytes()[1] - b'0';
            Square::at(f, r)
        };
        let (from, to) = (sq(&uci[0..2]), sq(&uci[2..4]));
        let piece = b.get(from).expect("a piece to move");
        let mut frames = Vec::new();
        let mut cur = b.clone();
        // Castling as two piece moves, king first.
        let rook = match (piece, uci) {
            (WhiteKing, "e1g1") => Some(("h1", "f1", WhiteRook)),
            (WhiteKing, "e1c1") => Some(("a1", "d1", WhiteRook)),
            (BlackKing, "e8g8") => Some(("h8", "f8", BlackRook)),
            (BlackKing, "e8c8") => Some(("a8", "d8", BlackRook)),
            _ => None,
        };
        if b.get(to).is_some() {
            cur.set(to, None);
            frames.push(cur.clone());
        }
        cur.set(from, None);
        frames.push(cur.clone());
        let placed = match uci.as_bytes().get(4) {
            Some(b'q') if piece == WhitePawn => WhiteQueen,
            Some(b'q') => BlackQueen,
            Some(b'n') if piece == WhitePawn => WhiteKnight,
            Some(b'n') => BlackKnight,
            _ => piece,
        };
        cur.set(to, Some(placed));
        frames.push(cur.clone());
        if let Some((rf, rt, r)) = rook {
            cur.set(sq(rf), None);
            frames.push(cur.clone());
            cur.set(sq(rt), Some(r));
            frames.push(cur.clone());
        }
        *b = cur;
        frames.iter().filter_map(|f| game.update(f)).collect()
    }

    fn legal(uci: &str, san: &str, status: Status) -> Ruling {
        Ruling::Legal {
            uci: uci.into(),
            san: san.into(),
            status,
        }
    }

    const ITALIAN: &str = "r1bqkb1r/pppp1ppp/2n2n2/4p3/2B1P3/5N2/PPPP1PPP/RNBQK2R w KQkq - 4 4";

    #[test]
    fn from_fen_is_in_sync_when_the_board_matches() {
        let game = RefereedGame::from_fen(ITALIAN).unwrap();
        let expected = Board::from_fen_placement(ITALIAN).unwrap();
        assert_eq!(game.expected_board(), expected);
        assert_eq!(game.turn(), Color::White);
        assert!(game.diff(&expected).is_empty());

        let game = game.with_board(&expected);
        assert!(game.in_sync());
        assert!(RefereedGame::from_fen("not a fen").is_err());
    }

    #[test]
    fn from_fen_out_of_sync_until_set_up() {
        use crate::board::Piece;
        let expected = Board::from_fen_placement(ITALIAN).unwrap();
        let mut b = Board::startpos();
        let mut game = RefereedGame::from_fen(ITALIAN).unwrap().with_board(&b);
        assert!(!game.in_sync());

        // The diff lists exactly the squares to fix, with expected vs actual.
        let diff = game.diff(&b);
        assert!(diff.contains(&(Square::at(4, 4), Some(Piece::WhitePawn), None))); // e4
        assert!(diff.contains(&(Square::at(4, 2), None, Some(Piece::WhitePawn)))); // e2
        assert_eq!(diff.len(), 12);

        // Fix squares one at a time; nothing is reported until the last one.
        let fixes: Vec<_> = diff.iter().map(|&(sq, want, _)| (sq, want)).collect();
        for &(sq, want) in &fixes[..fixes.len() - 1] {
            b.set(sq, want);
            assert_eq!(game.update(&b), None);
        }
        let &(sq, want) = fixes.last().unwrap();
        b.set(sq, want);
        assert_eq!(game.update(&b), Some(Ruling::BackInSync));
        assert!(game.in_sync());
        assert_eq!(b, expected);

        // …and the game then referees from that position (White to move).
        assert_eq!(play(&mut game, &mut b, "e1g1"), vec![legal("e1g1", "O-O", Status::Normal)]);
    }

    #[test]
    fn expect_move_reports_expected_played() {
        let mut b = Board::from_fen_placement(ITALIAN).unwrap();
        let mut game = RefereedGame::from_fen(ITALIAN).unwrap().with_board(&b);
        assert_eq!(play(&mut game, &mut b, "c4f7"), vec![legal("c4f7", "Bxf7+", Status::Check)]);

        // Black's reply is announced; the position is already past it.
        assert_eq!(game.san("e8f7").as_deref(), Some("Kxf7"));
        assert_eq!(game.san("e8d8"), None); // own queen on d8
        assert_eq!(game.expect_move("e8f7").unwrap(), "Kxf7");
        assert_eq!(game.expected_move(), Some("e8f7"));
        assert!(!game.in_sync());
        assert_eq!(game.turn(), Color::White);
        assert_eq!(game.diff(&b).len(), 2);
        assert!(game.expect_move("e8f7").is_err(), "one expectation at a time");

        assert_eq!(
            play(&mut game, &mut b, "e8f7"),
            vec![Ruling::ExpectedPlayed {
                uci: "e8f7".into(),
                san: "Kxf7".into(),
                status: Status::Normal
            }]
        );
        assert!(game.in_sync());
        assert_eq!(game.expected_move(), None);
        // White continues normally.
        assert_eq!(play(&mut game, &mut b, "f3g5"), vec![legal("f3g5", "Ng5+", Status::Check)]);
    }

    #[test]
    fn wrong_move_then_fixed() {
        let mut b = Board::from_fen_placement(ITALIAN).unwrap();
        let mut game = RefereedGame::from_fen(ITALIAN).unwrap().with_board(&b);
        assert_eq!(game.expect_move("e1g1").unwrap(), "O-O");

        // The player castles queenside-ish… no: plays d2d3 instead.
        assert_eq!(
            play(&mut game, &mut b, "d2d3"),
            vec![Ruling::WrongMove {
                expected: "e1g1".into(),
                got: "d2d3".into()
            }]
        );
        assert!(!game.in_sync());
        assert_eq!(game.expected_move(), Some("e1g1"));

        // Taking it back is silent, then castling resolves the expectation.
        assert_eq!(play(&mut game, &mut b, "d3d2"), vec![]);
        assert_eq!(
            play(&mut game, &mut b, "e1g1"),
            vec![Ruling::ExpectedPlayed {
                uci: "e1g1".into(),
                san: "O-O".into(),
                status: Status::Normal
            }]
        );
        assert!(game.in_sync());
    }

    #[test]
    fn referees_legal_illegal_and_resync_from_mid_game_fen() {
        let mut b = Board::from_fen_placement(ITALIAN).unwrap();
        let mut game = RefereedGame::from_fen(ITALIAN).unwrap().with_board(&b);

        // Illegal: the bishop can't move like a knight.
        assert_eq!(
            play(&mut game, &mut b, "c4d6"),
            vec![Ruling::Illegal {
                uci: "c4d6".into(),
                reason: IllegalReason::IllegalMove,
                piece: Piece::WhiteBishop,
            }]
        );
        assert!(!game.in_sync());
        assert_eq!(play(&mut game, &mut b, "d6c4"), vec![Ruling::BackInSync]);

        assert_eq!(play(&mut game, &mut b, "d2d3"), vec![legal("d2d3", "d3", Status::Normal)]);
        assert_eq!(game.turn(), Color::Black);
        assert_eq!(play(&mut game, &mut b, "f8c5"), vec![legal("f8c5", "Bc5", Status::Normal)]);
    }

    #[test]
    fn castling_is_accepted_in_both_uci_spellings() {
        for spelling in ["e1g1", "e1h1"] {
            let mut b = Board::from_fen_placement(ITALIAN).unwrap();
            let mut game = RefereedGame::from_fen(ITALIAN).unwrap().with_board(&b);
            assert_eq!(game.expect_move(spelling).unwrap(), "O-O");
            let rulings = play(&mut game, &mut b, "e1g1");
            assert!(
                matches!(&rulings[..], [Ruling::ExpectedPlayed { uci, san, .. }] if uci == spelling && san == "O-O"),
                "{spelling}: {rulings:?}"
            );
            assert_eq!(b.get(Square::at(5, 1)), Some(crate::board::Piece::WhiteRook)); // f1
        }
    }

    #[test]
    fn promotion() {
        let fen = "8/4P2k/8/8/8/8/8/4K3 w - - 0 1";
        let mut b = Board::from_fen_placement(fen).unwrap();
        let mut game = RefereedGame::from_fen(fen).unwrap().with_board(&b);
        assert_eq!(game.expect_move("e7e8q").unwrap(), "e8=Q");
        assert_eq!(
            play(&mut game, &mut b, "e7e8q"),
            vec![Ruling::ExpectedPlayed {
                uci: "e7e8q".into(),
                san: "e8=Q".into(),
                status: Status::Normal
            }]
        );
        // And as a freely refereed move, underpromoting to a knight.
        let mut b = Board::from_fen_placement(fen).unwrap();
        let mut game = RefereedGame::from_fen(fen).unwrap().with_board(&b);
        assert_eq!(
            play(&mut game, &mut b, "e7e8n"),
            vec![legal("e7e8n", "e8=N", Status::DrawInsufficientMaterial)]
        );
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
