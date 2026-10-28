//! Keep a [`RefereedGame`] in step with a game played on a server.
//!
//! An online game (Lichess's Board API, say) is a stream of states, each
//! carrying the full move list so far. [`RemoteGame`] reconciles that list
//! with the position refereed on the physical board, with no transport of
//! its own:
//!
//! - a new opponent ply becomes the move the board is *expected* to show
//!   ([`RefereedGame::expect_move`] → [`RemoteEvent::OpponentMoved`]);
//! - a ply you played on the board and sent to the server is consumed
//!   silently when the server echoes it;
//! - anything else — a move the server has that you never played here
//!   (joining mid-game), a takeback, or a move of yours the server replaced
//!   — swaps the local position for the server's and leaves the board out of
//!   sync until it is set up to match ([`RemoteEvent::Replaced`],
//!   [`RemoteEvent::Contradicted`]); [`RefereedGame::diff`] lists the squares
//!   to fix.
//!
//! Castling may arrive spelled `e1g1` or king-takes-rook `e1h1`; both are
//! normalised before comparison, so they never count as a disagreement.

use shakmaty::{uci::UciMove, CastlingMode, Chess, Position};

use crate::board::{Board, Color, Square};
use crate::rules::{IllegalReason, RefereedGame, RulesError, Ruling};

/// What a server state meant for the local game.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RemoteEvent {
    /// The opponent played a move; the game now waits to see it on the board.
    OpponentMoved {
        /// The move in UCI (normalised: castling as `e1g1`).
        uci: String,
        /// The move in SAN, with check/mate suffix.
        san: String,
    },
    /// The server has a different move where you sent `sent`: the local
    /// position was replaced by the server's and the board is out of sync.
    Contradicted {
        /// Ply number, 1-based.
        ply: usize,
        /// The move you sent (UCI).
        sent: String,
        /// The move the server has instead (UCI).
        server: String,
    },
    /// The server's move list replaced the local game (a reconnect, a
    /// takeback, or a move made somewhere else); the board must be set up to
    /// the new position before play continues.
    Replaced {
        /// Plies in the position now expected on the board.
        plies: usize,
    },
}

/// A refereed game mirrored from a server, for one player.
#[derive(Clone)]
pub struct RemoteGame {
    game: RefereedGame,
    me: Color,
    initial: Chess,
    /// Plies applied to the local position, in normalised UCI: the server's
    /// moves, plus up to one move of ours it hasn't echoed yet.
    local: Vec<String>,
    /// How many trailing `local` plies are ours and still unconfirmed (0 or 1).
    unconfirmed: usize,
    /// The last physical board we saw, so a replaced position can be checked
    /// against it.
    physical: Board,
}

impl RemoteGame {
    /// A game for the player with colour `me`, from `initial_fen` (`None` or
    /// `"startpos"` for the standard start). Assumes the board is empty until
    /// [`with_board`](Self::with_board) or [`update`](Self::update) says
    /// otherwise.
    pub fn new(initial_fen: Option<&str>, me: Color) -> Result<RemoteGame, RulesError> {
        let initial = match initial_fen {
            None | Some("startpos") | Some("") => Chess::default(),
            Some(fen) => RefereedGame::from_fen(fen)?.position().clone(),
        };
        Ok(RemoteGame {
            game: RefereedGame::from_position(initial.clone()),
            me,
            initial,
            local: Vec::new(),
            unconfirmed: 0,
            physical: Board::empty(),
        })
    }

    /// Compare the game with the physical board (see [`RefereedGame::with_board`]).
    pub fn with_board(mut self, physical: &Board) -> RemoteGame {
        self.physical = physical.clone();
        self.game = self.game.with_board(physical);
        self
    }

    /// The refereed game (position, expected board, diff, …).
    pub fn game(&self) -> &RefereedGame {
        &self.game
    }

    /// The colour we play.
    pub fn me(&self) -> Color {
        self.me
    }

    /// The plies applied locally so far, in normalised UCI.
    pub fn moves(&self) -> &[String] {
        &self.local
    }

    /// The move we played and sent that the server hasn't echoed yet, if any.
    pub fn unconfirmed(&self) -> Option<&str> {
        (self.unconfirmed > 0).then(|| self.local[self.local.len() - 1].as_str())
    }

    /// Whether it is our move: our colour is to move, no opponent move is
    /// waiting to be played on the board, and nothing of ours awaits an echo.
    pub fn my_turn(&self) -> bool {
        self.game.turn() == self.me && self.game.expected_move().is_none() && self.unconfirmed == 0
    }

    /// Feed the latest physical board (see [`RefereedGame::update`]). A
    /// [`Ruling::Legal`] move is ours and should be sent to the server; it is
    /// remembered as unconfirmed until [`sync`](Self::sync) sees the echo or
    /// [`rejected`](Self::rejected) rolls it back. A piece of the opponent's
    /// colour moved while we wait for them is refused as
    /// [`IllegalReason::NotYourTurn`].
    pub fn update(&mut self, physical: &Board) -> Option<Ruling> {
        self.physical = physical.clone();
        let mover = self.game.turn();
        let ruling = self.game.update(physical)?;
        if let Ruling::Legal { uci, .. } = &ruling {
            if mover != self.me {
                // The referee accepted it as a legal move for the other side,
                // but that side is played on the server. Undo it locally.
                let uci = uci.clone();
                self.rebuild();
                let from = parse_square(&uci[0..2]);
                let piece = from.and_then(|sq| self.game.expected_board().get(sq));
                return Some(Ruling::Illegal {
                    uci,
                    reason: IllegalReason::NotYourTurn,
                    piece: piece.unwrap_or(crate::board::Piece::WhitePawn),
                });
            }
            self.local.push(uci.clone());
            self.unconfirmed += 1;
        }
        Some(ruling)
    }

    /// Reconcile with the server's move list so far (space-separated UCI,
    /// from the initial position). Returns what changed for the player, if
    /// anything. Fails only if the server's list is not a legal sequence.
    pub fn sync(&mut self, moves: &str) -> Result<Option<RemoteEvent>, RulesError> {
        let mut pos = self.initial.clone();
        let mut server = Vec::new();
        for (i, s) in moves.split_whitespace().enumerate() {
            let m = UciMove::from_ascii(s.as_bytes())
                .map_err(|e| RulesError(format!("bad UCI move {s:?} from server: {e}")))?
                .to_move(&pos)
                .map_err(|_| RulesError(format!("server move {s} (ply {}) is not legal", i + 1)))?;
            server.push(m.to_uci(CastlingMode::Standard).to_string());
            pos = pos
                .play(m)
                .map_err(|_| RulesError(format!("server move {s} (ply {}) is not legal", i + 1)))?;
        }

        let confirmed = self.local.len() - self.unconfirmed;
        let common = self.local.iter().zip(&server).take_while(|(a, b)| a == b).count();
        if common < confirmed {
            // The server disagrees with history it had already confirmed:
            // a takeback, or something happened outside this board.
            let plies = server.len();
            self.replace(server, pos);
            return Ok(Some(RemoteEvent::Replaced { plies }));
        }
        if self.unconfirmed > 0 {
            let ply = confirmed;
            match server.get(ply) {
                None => return Ok(None), // our move isn't echoed yet
                Some(theirs) if *theirs != self.local[ply] => {
                    let event = RemoteEvent::Contradicted {
                        ply: ply + 1,
                        sent: self.local[ply].clone(),
                        server: theirs.clone(),
                    };
                    self.replace(server, pos);
                    return Ok(Some(event));
                }
                Some(_) => self.unconfirmed = 0,
            }
        }

        // From here the server list starts with everything we have locally.
        match &server[self.local.len()..] {
            [] => Ok(None),
            [one] if self.game.turn() != self.me && self.game.expected_move().is_none() => {
                let san = self.game.expect_move(one)?;
                self.local.push(one.clone());
                Ok(Some(RemoteEvent::OpponentMoved {
                    uci: one.clone(),
                    san,
                }))
            }
            _ => {
                let plies = server.len();
                self.replace(server, pos);
                Ok(Some(RemoteEvent::Replaced { plies }))
            }
        }
    }

    /// The server refused the move we sent: roll it back. The board still
    /// shows it, so the game is out of sync until the piece is put back.
    /// Returns the move that was rolled back.
    pub fn rejected(&mut self) -> Option<String> {
        if self.unconfirmed == 0 {
            return None;
        }
        let uci = self.local.pop();
        self.unconfirmed -= 1;
        self.rebuild();
        uci
    }

    fn replace(&mut self, moves: Vec<String>, pos: Chess) {
        self.local = moves;
        self.unconfirmed = 0;
        self.game = RefereedGame::from_position(pos).with_board(&self.physical);
    }

    /// Rebuild the refereed game from the local move list.
    fn rebuild(&mut self) {
        let mut pos = self.initial.clone();
        for uci in &self.local {
            let m = UciMove::from_ascii(uci.as_bytes())
                .expect("local moves are normalised UCI")
                .to_move(&pos)
                .expect("local moves were legal when applied");
            pos = pos.play(m).expect("local moves were legal when applied");
        }
        self.game = RefereedGame::from_position(pos).with_board(&self.physical);
    }
}

fn parse_square(s: &str) -> Option<Square> {
    let b = s.as_bytes();
    if b.len() != 2 || !(b'a'..=b'h').contains(&b[0]) || !(b'1'..=b'8').contains(&b[1]) {
        return None;
    }
    Some(Square::at(b[0] - b'a', b[1] - b'0'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::board::Piece;
    use crate::rules::Status;

    const ITALIAN: &str = "r1bqkb1r/pppp1ppp/2n2n2/4p3/2B1P3/5N2/PPPP1PPP/RNBQK2R w KQkq - 4 4";

    fn sq(s: &str) -> Square {
        parse_square(s).unwrap()
    }

    /// Play `uci` on `b` like a human (lift, then place; captures lift the
    /// victim first; castling moves the rook after the king), feeding every
    /// intermediate board to `game`. Returns the rulings in order.
    fn play(game: &mut RemoteGame, b: &mut Board, uci: &str) -> Vec<Ruling> {
        let (from, to) = (sq(&uci[0..2]), sq(&uci[2..4]));
        let piece = b.get(from).expect("a piece to move");
        let mut frames = Vec::new();
        let mut cur = b.clone();
        let rook = match (piece, uci) {
            (Piece::WhiteKing, "e1g1") => Some(("h1", "f1", Piece::WhiteRook)),
            (Piece::BlackKing, "e8g8") => Some(("h8", "f8", Piece::BlackRook)),
            _ => None,
        };
        if b.get(to).is_some() {
            cur.set(to, None);
            frames.push(cur.clone());
        }
        cur.set(from, None);
        frames.push(cur.clone());
        let placed = match uci.as_bytes().get(4) {
            Some(b'q') if piece == Piece::WhitePawn => Piece::WhiteQueen,
            Some(b'q') => Piece::BlackQueen,
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

    /// Set the physical board to the expected position and report the ruling.
    fn set_up(game: &mut RemoteGame, b: &mut Board) -> Option<Ruling> {
        *b = game.game().expected_board();
        game.update(b)
    }

    fn opponent(uci: &str, san: &str) -> Option<RemoteEvent> {
        Some(RemoteEvent::OpponentMoved {
            uci: uci.into(),
            san: san.into(),
        })
    }

    #[test]
    fn opponent_ply_is_expected_then_played_and_my_reply_is_echoed() {
        let mut b = Board::startpos();
        let mut game = RemoteGame::new(None, Color::Black).unwrap().with_board(&b);
        assert!(game.game().in_sync());
        assert!(!game.my_turn());
        assert_eq!(game.sync("").unwrap(), None);

        assert_eq!(game.sync("e2e4").unwrap(), opponent("e2e4", "e4"));
        assert_eq!(game.game().expected_move(), Some("e2e4"));
        assert!(!game.my_turn());
        assert!(matches!(
            play(&mut game, &mut b, "e2e4")[..],
            [Ruling::ExpectedPlayed { ref san, .. }] if san == "e4"
        ));
        assert!(game.my_turn());
        assert_eq!(game.sync("e2e4").unwrap(), None, "same state again is a no-op");

        assert!(matches!(play(&mut game, &mut b, "e7e5")[..], [Ruling::Legal { ref uci, .. }] if uci == "e7e5"));
        assert_eq!(game.unconfirmed(), Some("e7e5"));
        assert!(!game.my_turn());
        assert_eq!(game.sync("e2e4").unwrap(), None, "not echoed yet");
        assert_eq!(game.sync("e2e4 e7e5").unwrap(), None, "echo consumed silently");
        assert_eq!(game.unconfirmed(), None);
        assert_eq!(game.moves(), ["e2e4", "e7e5"]);

        assert_eq!(game.sync("e2e4 e7e5 g1f3").unwrap(), opponent("g1f3", "Nf3"));
    }

    #[test]
    fn wrong_move_then_fixed() {
        let mut b = Board::startpos();
        let mut game = RemoteGame::new(None, Color::Black).unwrap().with_board(&b);
        assert_eq!(game.sync("e2e4").unwrap(), opponent("e2e4", "e4"));
        assert_eq!(
            play(&mut game, &mut b, "d2d4"),
            vec![Ruling::WrongMove {
                expected: "e2e4".into(),
                got: "d2d4".into()
            }]
        );
        assert_eq!(play(&mut game, &mut b, "d4d2"), vec![]);
        assert!(matches!(play(&mut game, &mut b, "e2e4")[..], [Ruling::ExpectedPlayed { .. }]));
        assert!(game.my_turn());
    }

    #[test]
    fn joining_mid_game_replaces_the_position_until_the_board_is_set_up() {
        let mut b = Board::startpos();
        let mut game = RemoteGame::new(None, Color::White).unwrap().with_board(&b);
        assert_eq!(
            game.sync("e2e4 e7e5 g1f3").unwrap(),
            Some(RemoteEvent::Replaced { plies: 3 })
        );
        assert!(!game.game().in_sync());
        assert_eq!(game.game().turn(), Color::Black);
        assert!(!game.my_turn());
        assert_eq!(game.game().diff(&b).len(), 6);
        assert_eq!(set_up(&mut game, &mut b), Some(Ruling::BackInSync));

        // The next state brings the opponent's reply, as usual.
        assert_eq!(game.sync("e2e4 e7e5 g1f3 b8c6").unwrap(), opponent("b8c6", "Nc6"));
        assert!(matches!(play(&mut game, &mut b, "b8c6")[..], [Ruling::ExpectedPlayed { .. }]));
        assert!(game.my_turn());
    }

    #[test]
    fn castling_in_both_server_spellings() {
        for spelling in ["e1g1", "e1h1"] {
            let mut b = Board::from_fen_placement(ITALIAN).unwrap();
            let mut game = RemoteGame::new(Some(ITALIAN), Color::Black).unwrap().with_board(&b);
            assert_eq!(game.sync(spelling).unwrap(), opponent("e1g1", "O-O"), "{spelling}");
            assert!(matches!(play(&mut game, &mut b, "e1g1")[..], [Ruling::ExpectedPlayed { ref san, .. }] if san == "O-O"));
            assert_eq!(b.get(sq("f1")), Some(Piece::WhiteRook));
        }
        // Our own castling, echoed by the server in the other spelling.
        let mut b = Board::from_fen_placement(ITALIAN).unwrap();
        let mut game = RemoteGame::new(Some(ITALIAN), Color::White).unwrap().with_board(&b);
        assert!(matches!(play(&mut game, &mut b, "e1g1")[..], [Ruling::Legal { ref uci, .. }] if uci == "e1g1"));
        assert_eq!(game.sync("e1h1").unwrap(), None);
        assert_eq!(game.unconfirmed(), None);
    }

    #[test]
    fn promotion() {
        let fen = "8/4P2k/8/8/8/8/8/4K3 w - - 0 1";
        let mut b = Board::from_fen_placement(fen).unwrap();
        let mut game = RemoteGame::new(Some(fen), Color::Black).unwrap().with_board(&b);
        assert_eq!(game.sync("e7e8q").unwrap(), opponent("e7e8q", "e8=Q"));
        assert!(matches!(play(&mut game, &mut b, "e7e8q")[..], [Ruling::ExpectedPlayed { .. }]));
        assert_eq!(b.get(sq("e8")), Some(Piece::WhiteQueen));

        let fen = "4k3/8/8/8/8/8/4p3/6K1 b - - 0 1";
        let mut b = Board::from_fen_placement(fen).unwrap();
        let mut game = RemoteGame::new(Some(fen), Color::Black).unwrap().with_board(&b);
        assert!(matches!(play(&mut game, &mut b, "e2e1q")[..], [Ruling::Legal { ref uci, ref status, .. }] if uci == "e2e1q" && *status == Status::Check));
        assert_eq!(game.unconfirmed(), Some("e2e1q"));
        assert_eq!(game.sync("e2e1q").unwrap(), None);
    }

    #[test]
    fn a_rejected_move_is_rolled_back_until_the_piece_is_put_back() {
        let mut b = Board::startpos();
        let mut game = RemoteGame::new(None, Color::White).unwrap().with_board(&b);
        assert!(game.my_turn());
        assert_eq!(game.rejected(), None);
        assert!(matches!(play(&mut game, &mut b, "e2e4")[..], [Ruling::Legal { .. }]));
        assert_eq!(game.rejected().as_deref(), Some("e2e4"));
        assert!(!game.game().in_sync());
        assert_eq!(game.moves(), Vec::<String>::new());
        assert_eq!(game.game().diff(&b).len(), 2);
        assert_eq!(play(&mut game, &mut b, "e4e2"), vec![Ruling::BackInSync]);
        assert!(game.my_turn());
        assert!(matches!(play(&mut game, &mut b, "d2d4")[..], [Ruling::Legal { ref uci, .. }] if uci == "d2d4"));
    }

    #[test]
    fn a_contradicted_move_replaces_the_position() {
        let mut b = Board::startpos();
        let mut game = RemoteGame::new(None, Color::White).unwrap().with_board(&b);
        play(&mut game, &mut b, "e2e4");
        assert_eq!(
            game.sync("d2d4").unwrap(),
            Some(RemoteEvent::Contradicted {
                ply: 1,
                sent: "e2e4".into(),
                server: "d2d4".into()
            })
        );
        assert_eq!(game.moves(), ["d2d4"]);
        assert!(!game.game().in_sync());
        assert_eq!(game.game().expected_board().get(sq("d4")), Some(Piece::WhitePawn));
        assert_eq!(set_up(&mut game, &mut b), Some(Ruling::BackInSync));
    }

    #[test]
    fn a_takeback_replaces_the_position() {
        let mut b = Board::startpos();
        let mut game = RemoteGame::new(None, Color::White).unwrap().with_board(&b);
        play(&mut game, &mut b, "e2e4");
        assert_eq!(game.sync("e2e4 e7e5").unwrap(), opponent("e7e5", "e5"));
        play(&mut game, &mut b, "e7e5");
        assert_eq!(game.sync("e2e4").unwrap(), Some(RemoteEvent::Replaced { plies: 1 }));
        assert_eq!(game.game().turn(), Color::Black);
        assert_eq!(game.game().diff(&b).len(), 2);
    }

    #[test]
    fn moving_the_opponents_piece_is_refused() {
        let mut b = Board::startpos();
        let mut game = RemoteGame::new(None, Color::White).unwrap().with_board(&b);
        play(&mut game, &mut b, "e2e4");
        // Impatient: plays Black's reply before the server does.
        assert_eq!(
            play(&mut game, &mut b, "e7e5"),
            vec![Ruling::Illegal {
                uci: "e7e5".into(),
                reason: IllegalReason::NotYourTurn,
                piece: Piece::BlackPawn,
            }]
        );
        assert_eq!(game.moves(), ["e2e4"]);
        assert_eq!(game.unconfirmed(), Some("e2e4"));
        assert_eq!(play(&mut game, &mut b, "e5e7"), vec![Ruling::BackInSync]);
        assert_eq!(game.sync("e2e4 e7e5").unwrap(), opponent("e7e5", "e5"));
    }

    #[test]
    fn illegal_server_moves_are_an_error() {
        let mut game = RemoteGame::new(None, Color::White).unwrap();
        assert!(game.sync("e2e5").is_err());
        assert!(game.sync("zz").is_err());
        assert!(RemoteGame::new(Some("not a fen"), Color::White).is_err());
    }
}
