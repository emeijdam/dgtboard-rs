//! What a session observes, and who shows it.
//!
//! `dgt lichess` and `dgt puzzle` drive a game on the physical board. What
//! they learn along the way — the opponent moved, the board is out of sync,
//! the clocks, the result — is reported as [`Notice`]s to a [`Ui`] rather
//! than printed, so the same session can run under the terminal ([`Terminal`]
//! prints the notices exactly as the CLI always has) or inside the 3D viewer,
//! which turns them into highlights and a HUD. Sessions never print.
//!
//! [`Notice::Snapshot`] is for renderers: the position the board should show,
//! what it does show, and where the last move and any check are, in DGT
//! square indices (0 = a8 … 63 = h1), after every change.

use std::io::{self, Write};

use crate::{Board, Color, IllegalReason, Piece, Square, Status};

/// Something a session wants shown.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Notice {
    /// A line of context: a header, a chat line, an offer, "Logged in as …".
    Info(String),
    /// A Lichess game started (or was rejoined).
    Game {
        /// Lichess game id.
        id: String,
        /// The colour the player has.
        me: Color,
        /// Opponent's name (with rating when known).
        opponent: String,
        /// E.g. `15+10`, or the speed when there is no clock.
        time_control: String,
        /// Rated or casual.
        rated: bool,
    },
    /// A puzzle was fetched.
    Puzzle {
        /// Lichess puzzle id.
        id: String,
        /// One-line header (id, rating, plays, themes).
        title: String,
        /// The game the puzzle comes from, if known.
        source: Option<String>,
        /// What the opponent just played, e.g. `Black just played Bxh2+ (e5h2).`
        last_move: Option<String>,
        /// The position to solve.
        fen: String,
        /// The side that solves.
        solver: Color,
    },
    /// Remaining time per side, in milliseconds; `ticking` is the side whose
    /// clock is running.
    Clocks {
        /// White's remaining time.
        white_ms: u64,
        /// Black's remaining time.
        black_ms: u64,
        /// Whose clock runs, if the game is on.
        ticking: Option<Color>,
    },
    /// It's the player's move.
    YourMove,
    /// The opponent played `san`; the board should show it next.
    Expect {
        /// The side that moved.
        by: Color,
        /// The move in UCI.
        uci: String,
        /// The move in SAN.
        san: String,
    },
    /// The expected move appeared on the board.
    Executed {
        /// The resulting status.
        status: Status,
    },
    /// A different move than the expected one appeared (both in SAN).
    WrongMove {
        /// What was played.
        got: String,
        /// What was expected.
        expected: String,
    },
    /// The player's move was accepted and sent to Lichess.
    Sent {
        /// The move in SAN.
        san: String,
        /// The resulting status.
        status: Status,
    },
    /// The player's move is the puzzle's solution (or mates).
    Correct {
        /// The move in SAN.
        san: String,
        /// The resulting status.
        status: Status,
    },
    /// The player's move isn't the puzzle's solution (both in SAN).
    Wrong {
        /// What was played.
        san: String,
        /// What the solution wants.
        expected: String,
    },
    /// The player's move isn't legal.
    Illegal {
        /// The move in UCI.
        uci: String,
        /// Why, when the referee could tell.
        reason: Option<IllegalReason>,
        /// The piece that moved, when known.
        piece: Option<Piece>,
    },
    /// Lichess refused the move the player made.
    Refused {
        /// The move in SAN.
        san: String,
        /// Lichess's reason, or the transport error.
        why: String,
    },
    /// The board must show `expected`; `diff` lists the squares that differ
    /// (empty: it does). Sent with every change while out of sync.
    Setup {
        /// The position to set up.
        expected: Board,
        /// `(square, wanted, actual)` for each differing square.
        diff: Vec<(Square, Option<Piece>, Option<Piece>)>,
        /// Whose move it is in that position.
        turn: Color,
        /// Plies played so far, when joining a game in progress.
        plies: Option<usize>,
        /// Whether this starts a new setup request (show the position) or
        /// just updates the list.
        announce: bool,
    },
    /// The game or puzzle ended; a result line.
    Over(String),
    /// What the session is waiting for ("Waiting for a game…").
    Waiting(String),
    /// The state for renderers, after every change.
    Snapshot {
        /// The position the board should show.
        expected: Board,
        /// What the board shows.
        physical: Board,
        /// The last move played, as DGT square indices.
        last_move: Option<(usize, usize)>,
        /// The king in check, as a DGT square index.
        check: Option<usize>,
        /// Whose move it is.
        turn: Color,
        /// The player's colour.
        me: Color,
    },
}

/// Something that shows [`Notice`]s.
pub trait Ui {
    /// Show (or record) a notice.
    fn notice(&mut self, notice: Notice);
}

/// The terminal: prints notices as the CLI always has, keeping the
/// "play it on the board …" prompt open until its move appears.
#[derive(Default)]
pub struct Terminal {
    /// The prompt line printed without a newline, if one is open.
    pending: Option<String>,
}

impl Terminal {
    /// A terminal with no prompt open.
    pub fn new() -> Terminal {
        Terminal::default()
    }

    /// Close an open prompt line before printing something else.
    fn end_pending(&mut self) {
        if self.pending.take().is_some() {
            println!();
        }
    }

    /// Re-print the open prompt after an interruption.
    fn show_pending(&self) {
        if let Some(p) = &self.pending {
            print!("{p}");
            let _ = io::stdout().flush();
        }
    }

    fn print(&self, notice: Notice) {
        use Notice::*;
        match notice {
            Info(s) | Waiting(s) => println!("{s}"),
            Game {
                id,
                me,
                opponent,
                time_control,
                rated,
            } => println!(
                "Game {id}: you are {me} vs. {opponent} · {time_control}{}",
                if rated { " rated" } else { "" }
            ),
            Puzzle {
                title,
                source,
                last_move,
                fen,
                ..
            } => {
                println!("{title}");
                if let Some(s) = source {
                    println!("{s}");
                }
                if let Some(l) = last_move {
                    println!("{l}");
                }
                println!();
                if let Ok(board) = Board::from_fen_placement(fen.split(' ').next().unwrap_or("")) {
                    print!("{board}");
                }
                println!("FEN: {fen}\n");
            }
            Clocks {
                white_ms, black_ms, ..
            } => println!("  ⏱ {} / {}", clock_text(white_ms), clock_text(black_ms)),
            YourMove => println!("  Your move."),
            Sent { san, status } => {
                println!("  → {san} sent");
                announce(status);
            }
            Correct { san, status } => {
                println!("  ✓ {san}");
                announce(status);
            }
            Wrong { san, expected } => println!("  ✗ {san} isn't it (expected {expected}) — put it back"),
            Illegal { uci, reason, piece } => match (reason, piece) {
                (Some(reason), Some(piece)) => println!(
                    "  ⚠ {uci} is not legal here — {}. Put the {} back.",
                    reason_text(reason),
                    piece.glyph()
                ),
                _ => println!("  ⚠ {uci} is not legal here — put the piece back"),
            },
            Refused { san, why } => println!("  ✗ Lichess refused {san}: {why} — put it back"),
            Setup {
                expected,
                diff,
                turn,
                plies,
                announce,
            } => {
                if announce {
                    println!("\n{expected}");
                    match plies {
                        Some(n) => println!("Set up this position on the board ({n} plies played, {turn} to move)."),
                        None => println!("Set up this position on the board."),
                    }
                }
                if !diff.is_empty() {
                    let item = |(sq, want, have): &(Square, Option<Piece>, Option<Piece>)| match (want, have) {
                        (Some(w), None) => format!("{sq}: place {}", w.glyph()),
                        (None, Some(h)) => format!("{sq}: remove {}", h.glyph()),
                        (Some(w), Some(h)) => format!("{sq}: {} → {}", h.glyph(), w.glyph()),
                        (None, None) => unreachable!("diff only lists differing squares"),
                    };
                    println!("  squares to fix: {}", diff.iter().map(item).collect::<Vec<_>>().join(", "));
                }
            }
            Over(s) => println!("\n{s}"),
            // Handled before `print`, or not for the terminal.
            Expect { .. } | Executed { .. } | WrongMove { .. } | Snapshot { .. } => {}
        }
    }
}

impl Ui for Terminal {
    fn notice(&mut self, notice: Notice) {
        match notice {
            Notice::Snapshot { .. } => {}
            Notice::Expect { by, san, .. } => {
                self.end_pending();
                self.pending = Some(format!("  ← {by} plays {san} — play it on the board …"));
                self.show_pending();
            }
            Notice::Executed { status } => {
                if self.pending.take().is_some() {
                    println!(" ✓");
                }
                announce(status);
            }
            Notice::WrongMove { got, expected } => {
                println!("\n  ⚠ you played {got}, expected {expected} — take it back");
                self.show_pending();
            }
            other => {
                self.end_pending();
                self.print(other);
                self.show_pending();
            }
        }
    }
}

/// Records every notice (for tests and for forwarding).
#[derive(Default)]
pub struct Recorder {
    /// The notices so far, in order.
    pub notices: Vec<Notice>,
}

impl Ui for Recorder {
    fn notice(&mut self, notice: Notice) {
        self.notices.push(notice);
    }
}

/// A friendly explanation of an [`IllegalReason`].
pub fn reason_text(reason: IllegalReason) -> &'static str {
    match reason {
        IllegalReason::NotYourTurn => "it's not your turn",
        IllegalReason::NoPieceThere => "there was no piece on that square",
        IllegalReason::MustGetOutOfCheck => "your king is in check",
        IllegalReason::OwnPieceOnTarget => "your own piece is already there",
        IllegalReason::IllegalMove => "that piece can't move like that",
    }
}

/// `mm:ss`, or `h:mm:ss` from an hour up.
pub fn clock_text(ms: u64) -> String {
    let s = ms / 1000;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60)
    } else {
        format!("{:02}:{:02}", s / 60, s % 60)
    }
}

/// Print the check / mate / draw banner for a status, as `dgt referee` does.
pub fn announce(status: Status) {
    match status {
        Status::Normal => {}
        Status::Check => println!("     + check"),
        Status::Checkmate { winner } => {
            println!("\n     ███  CHECKMATE — {winner} wins  ███\n");
        }
        Status::Stalemate => println!("\n     ▒▒▒  STALEMATE — draw  ▒▒▒\n"),
        Status::DrawInsufficientMaterial => {
            println!("\n     ▒▒▒  DRAW — insufficient material  ▒▒▒\n");
        }
    }
}

/// `e2e4` → the DGT indices of e2 and e4, for [`Notice::Snapshot`].
pub fn uci_squares(uci: &str) -> Option<(usize, usize)> {
    let sq = |s: &str| {
        let b = s.as_bytes();
        if b.len() != 2 || !(b'a'..=b'h').contains(&b[0]) || !(b'1'..=b'8').contains(&b[1]) {
            return None;
        }
        Some(Square::at(b[0] - b'a', b[1] - b'0').0 as usize)
    };
    if uci.len() < 4 {
        return None;
    }
    Some((sq(&uci[0..2])?, sq(&uci[2..4])?))
}
