//! Run a Lichess game or puzzle session inside the window (native only).
//!
//! The session itself lives in the `dgtboard` crate (`lichess_game`,
//! `puzzle`) and owns the board — the serial port, or the keyboard
//! simulation — on its own thread. It reports what happens as
//! [`Notice`]s; [`WindowUi`] forwards those over a channel for the Bevy
//! systems to turn into highlights and a HUD, and pushes every
//! [`Notice::Snapshot`]'s physical board into the shared byte [`QUEUE`] as a
//! synthetic dump, so the pieces track the board exactly as in free play
//! (one decoder, one path). Commands from the window (`resign`, Enter, `q`)
//! go back through the same line channel the CLI uses for stdin — and stdin
//! is forwarded into it too, so typing in the terminal still works.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Mutex;
use std::thread;

use anyhow::Context;
use dgtboard::lichess::Lichess;
use dgtboard::lichess_game::{self, Start};
use dgtboard::puzzle::{self, stdin_lines, BoardInput, PuzzleSource};
use dgtboard::ui::{uci_squares, Notice, Ui};

use super::{synthetic_board_dump, QUEUE};

/// What to run in the window.
#[derive(Clone, Debug)]
pub enum Mode {
    /// A Lichess game (wait for one, challenge Stockfish, or seek).
    Lichess {
        /// How to get the game.
        start: Start,
        /// Our colour for `--ai` / `--seek`: `white`, `black` or `random`.
        color: String,
    },
    /// Lichess puzzles.
    Puzzle(PuzzleSource),
}

/// How to reach the board and Lichess.
#[derive(Clone, Debug, Default)]
pub struct Connection {
    /// Serial port path; auto-detected when `None`.
    pub port: Option<String>,
    /// Read the board rotated 180°.
    pub flip: bool,
    /// No hardware: UCI moves typed on stdin.
    pub keyboard: bool,
    /// File holding the Lichess token (else `$LICHESS_TOKEN`, else anonymous).
    pub token_file: Option<PathBuf>,
}

/// The window's end of a running session.
pub struct SessionHandle {
    /// What the session reports, in order (behind a mutex only because a
    /// Bevy resource must be `Sync`; one system drains it).
    pub notices: Mutex<Receiver<Notice>>,
    /// Commands for the session: `resign`, `abort`, `board`, an empty line
    /// for Enter, `q` to quit — and, in keyboard mode, UCI moves.
    pub lines: Sender<String>,
}

/// The sink the session reports to: forwards notices to the window and the
/// physical board to the decoder.
struct WindowUi(Sender<Notice>);

impl Ui for WindowUi {
    fn notice(&mut self, notice: Notice) {
        if let Notice::Snapshot { physical, .. } = &notice {
            QUEUE.lock().unwrap().extend_from_slice(&synthetic_board_dump(physical));
        }
        // The window going away is the only reason this fails; the session
        // then just runs on until the user quits.
        let _ = self.0.send(notice);
    }
}

/// Start the session on its own thread. Never blocks; failures arrive as
/// [`Notice::Info`] lines (and on stderr).
pub fn spawn(mode: Mode, connection: Connection) -> SessionHandle {
    let (notices_tx, notices) = mpsc::channel();
    let (lines, lines_rx) = mpsc::channel::<String>();

    // Whatever is typed in the terminal counts as a command too.
    let stdin = stdin_lines();
    let forward = lines.clone();
    thread::spawn(move || {
        for line in stdin {
            if forward.send(line).is_err() {
                break;
            }
        }
    });

    thread::Builder::new()
        .name("dgt-session".into())
        .spawn(move || {
            let mut ui = WindowUi(notices_tx);
            if let Err(e) = run(mode, connection, lines_rx, &mut ui) {
                eprintln!("dgt3d: {e:#}");
                ui.notice(Notice::Info(format!("✗ {e:#}")));
            }
        })
        .expect("spawn session thread");
    SessionHandle {
        notices: Mutex::new(notices),
        lines,
    }
}

fn run(mode: Mode, connection: Connection, lines: Receiver<String>, ui: &mut dyn Ui) -> anyhow::Result<()> {
    let mut input = if connection.keyboard {
        BoardInput::keyboard_with_lines(lines)
    } else {
        BoardInput::open_serial_with_lines(connection.port, connection.flip, lines)?
    };
    let lichess = Lichess::from_token_file_or_env(connection.token_file.as_deref())
        .context("reading the Lichess token file")?;
    match mode {
        Mode::Lichess { start, color } => {
            lichess_game::run_with(&mut input, &lichess, lichess_game::Options { start, color }, ui)
        }
        Mode::Puzzle(source) => puzzle::run_with(&mut input, &lichess, source, ui),
    }
}

// ------------------------------------------------------------- in the app

use bevy::prelude::*;

use super::overlay::{self, Clocks, Hud, Instant, Mark, Overlay};
use dgtboard::ui::{clock_text, reason_text};
use dgtboard::{Board, Status};

/// The running session, as a Bevy resource.
#[derive(Resource)]
pub struct SessionRes {
    handle: SessionHandle,
    /// What the overlay is built from, accumulated from the notices.
    state: Bridge,
    /// When `R` was last pressed, for the two-press resign.
    resign_armed: Option<Instant>,
}

impl SessionRes {
    pub fn new(handle: SessionHandle) -> SessionRes {
        SessionRes {
            handle,
            state: Bridge::default(),
            resign_armed: None,
        }
    }
}

/// The pieces of an [`Overlay`], kept between notices because each notice
/// changes only one of them.
#[derive(Default)]
struct Bridge {
    title: String,
    line: String,
    banner: Option<String>,
    clocks: Option<Clocks>,
    last_move: Option<(usize, usize)>,
    check: Option<usize>,
    /// The opponent's move the board should show next.
    expect: Option<(usize, usize)>,
    /// Squares a resync needs touched.
    fix: Vec<(usize, Mark)>,
    /// The position the board should show, for the ghost's piece.
    expected: Board,
}

impl Bridge {
    /// Fold a notice in.
    fn apply(&mut self, notice: Notice) {
        use Notice::*;
        match notice {
            Info(s) => {
                let s = s.trim();
                if !s.is_empty() {
                    self.line = s.to_string();
                }
            }
            Waiting(s) => {
                self.line = s;
                self.clocks = None;
            }
            Game {
                opponent,
                time_control,
                rated,
                me,
                ..
            } => {
                self.title = format!(
                    "{opponent} · {time_control}{} · you are {me}",
                    if rated { " rated" } else { "" }
                );
                self.reset_game();
            }
            Puzzle {
                title, last_move, ..
            } => {
                self.title = title;
                self.line = last_move.unwrap_or_default();
                self.reset_game();
                self.clocks = None;
            }
            Clocks {
                white_ms,
                black_ms,
                ticking,
            } => {
                self.clocks = Some(overlay::Clocks {
                    white_ms,
                    black_ms,
                    ticking,
                    since: Instant::now(),
                });
            }
            YourMove => self.line = "Your move.".into(),
            Expect { by, uci, san } => {
                self.expect = uci_squares(&uci);
                self.line = format!("← {by} plays {san} — play it on the board …");
            }
            Executed { status } => {
                self.expect = None;
                self.line = "✓".into();
                self.status(status);
            }
            WrongMove { got, expected } => {
                self.line = format!("⚠ you played {got}, expected {expected} — take it back");
            }
            Sent { san, status } => {
                self.line = format!("→ {san} sent");
                self.status(status);
            }
            Correct { san, status } => {
                self.line = format!("✓ {san}");
                self.status(status);
            }
            Wrong { san, expected } => {
                self.line = format!("✗ {san} isn't it (expected {expected}) — put it back");
            }
            Illegal { uci, reason, .. } => {
                self.line = match reason {
                    Some(r) => format!("⚠ {uci} is not legal — {}", reason_text(r)),
                    None => format!("⚠ {uci} is not legal here — put the piece back"),
                };
            }
            Refused { san, why } => self.line = format!("✗ Lichess refused {san}: {why} — put it back"),
            Setup {
                diff, announce, ..
            } => {
                self.fix = diff
                    .iter()
                    .map(|(sq, want, have)| {
                        let mark = match (want, have) {
                            (Some(_), None) => Mark::Place,
                            (None, Some(_)) => Mark::Remove,
                            _ => Mark::Swap,
                        };
                        (sq.0 as usize, mark)
                    })
                    .collect();
                if announce || !self.fix.is_empty() {
                    self.line = format!(
                        "Set up this position on the board — {} square{} to fix",
                        self.fix.len(),
                        if self.fix.len() == 1 { "" } else { "s" }
                    );
                }
            }
            Over(s) => {
                self.banner = Some(s);
                self.expect = None;
                if let Some(c) = &mut self.clocks {
                    c.ticking = None;
                }
            }
            Snapshot {
                expected,
                last_move,
                check,
                ..
            } => {
                self.expected = expected;
                self.last_move = last_move;
                self.check = check;
            }
        }
    }

    /// A new game or puzzle: nothing from the previous one carries over.
    fn reset_game(&mut self) {
        self.banner = None;
        self.expect = None;
        self.fix.clear();
        self.last_move = None;
        self.check = None;
    }

    /// Check and game-ending statuses, as the terminal's banner.
    fn status(&mut self, status: Status) {
        match status {
            Status::Normal => {}
            Status::Check => self.line.push_str("  +"),
            Status::Checkmate { winner } => self.banner = Some(format!("Checkmate — {winner} wins")),
            Status::Stalemate => self.banner = Some("Stalemate — draw".into()),
            Status::DrawInsufficientMaterial => self.banner = Some("Draw — insufficient material".into()),
        }
    }

    /// The overlay for the current state.
    fn overlay(&self) -> Overlay {
        let mut marks = std::collections::HashMap::new();
        if let Some((from, to)) = self.last_move {
            marks.insert(from, Mark::LastFrom);
            marks.insert(to, Mark::LastTo);
        }
        if let Some((from, to)) = self.expect {
            marks.insert(from, Mark::ExpectFrom);
            marks.insert(to, Mark::ExpectTo);
        }
        if let Some(sq) = self.check {
            marks.insert(sq, Mark::Check);
        }
        for &(sq, mark) in &self.fix {
            marks.insert(sq, mark);
        }
        // The ghost is the moved piece on its destination: that square in the
        // expected position (or, until the snapshot catches up, its origin).
        let ghost = self.expect.and_then(|(from, to)| {
            let at = |idx: usize| self.expected.squares.get(idx).copied().flatten();
            at(to).or_else(|| at(from)).map(|p| (to, p))
        });
        Overlay {
            marks,
            ghost,
            hud: Hud {
                title: self.title.clone(),
                clocks: self.clocks,
                line: self.line.clone(),
                banner: self.banner.clone(),
            },
        }
    }
}

/// Drain the session's notices into the overlay.
pub fn notices(session: Option<ResMut<SessionRes>>, mut overlay: ResMut<Overlay>) {
    let Some(mut session) = session else {
        return;
    };
    let mut changed = false;
    loop {
        let next = session.handle.notices.lock().unwrap().try_recv();
        let Ok(notice) = next else { break };
        if !matches!(notice, Notice::Snapshot { .. }) {
            info!("session: {notice:?}");
        }
        session.state.apply(notice);
        changed = true;
    }
    if changed {
        *overlay = session.state.overlay();
    }
    // Disarm a lone `R` after two seconds.
    if session
        .resign_armed
        .is_some_and(|t| t.elapsed().as_secs_f32() > 2.0)
    {
        session.resign_armed = None;
        session.state.line = "Resign cancelled.".into();
        *overlay = session.state.overlay();
    }
}

/// Game keys: `R` twice within two seconds resigns, `A` aborts, `Enter` is
/// Enter (next puzzle), `Esc` / `Q` quits — leaving a Lichess game running,
/// exactly as the CLI does.
pub fn keys(
    keys: Res<ButtonInput<KeyCode>>,
    session: Option<ResMut<SessionRes>>,
    mut overlay: ResMut<Overlay>,
    mut exit: EventWriter<AppExit>,
) {
    let Some(mut session) = session else {
        return;
    };
    let send = |session: &SessionRes, line: &str| {
        let _ = session.handle.lines.send(line.to_string());
    };
    if keys.just_pressed(KeyCode::KeyR) {
        if session.resign_armed.take().is_some() {
            send(&session, "resign");
        } else {
            session.resign_armed = Some(Instant::now());
            session.state.line = "Press R again to resign.".into();
            *overlay = session.state.overlay();
        }
    }
    if keys.just_pressed(KeyCode::KeyA) {
        send(&session, "abort");
    }
    if keys.just_pressed(KeyCode::Enter) {
        send(&session, "");
    }
    if keys.just_pressed(KeyCode::Escape) || keys.just_pressed(KeyCode::KeyQ) {
        send(&session, "q");
        exit.send(AppExit::Success);
    }
}

/// The clock text for the HUD, shared with the CLI's formatting.
#[allow(dead_code)]
pub fn clock(ms: u64) -> String {
    clock_text(ms)
}

#[cfg(test)]
mod tests {
    use super::*;
    use dgtboard::{Color, Piece, Square};

    fn sq(name: &str) -> usize {
        overlay::square(name).unwrap()
    }

    /// An opponent's move becomes expect marks and a ghost on its destination;
    /// seeing it on the board clears them; a result becomes the banner.
    #[test]
    fn notices_become_an_overlay() {
        let mut b = Bridge::default();
        b.apply(Notice::Game {
            id: "g".into(),
            me: Color::White,
            opponent: "Stockfish level 1".into(),
            time_control: "15+10".into(),
            rated: false,
        });
        let mut expected = Board::startpos();
        expected.set(Square::at(4, 2), None);
        expected.set(Square::at(4, 4), Some(Piece::WhitePawn));
        b.apply(Notice::Snapshot {
            expected: expected.clone(),
            physical: expected.clone(),
            last_move: uci_squares("e2e4"),
            check: None,
            turn: Color::Black,
            me: Color::White,
        });
        b.apply(Notice::Expect {
            by: Color::Black,
            uci: "g8f6".into(),
            san: "Nf6".into(),
        });
        let o = b.overlay();
        assert_eq!(o.marks.get(&sq("e2")), Some(&Mark::LastFrom));
        assert_eq!(o.marks.get(&sq("e4")), Some(&Mark::LastTo));
        assert_eq!(o.marks.get(&sq("g8")), Some(&Mark::ExpectFrom));
        assert_eq!(o.marks.get(&sq("f6")), Some(&Mark::ExpectTo));
        // The snapshot predates the knight move, so the ghost takes the piece from g8.
        assert_eq!(o.ghost, Some((sq("f6"), Piece::BlackKnight)));
        assert!(o.hud.title.starts_with("Stockfish level 1 · 15+10"));
        assert!(o.hud.line.contains("Nf6"));

        b.apply(Notice::Executed { status: Status::Normal });
        let o = b.overlay();
        assert_eq!(o.ghost, None);
        assert!(!o.marks.values().any(|m| matches!(m, Mark::ExpectFrom | Mark::ExpectTo)));

        b.apply(Notice::Setup {
            expected: expected.clone(),
            diff: vec![(Square::at(3, 4), Some(Piece::WhitePawn), None)],
            turn: Color::White,
            plies: None,
            announce: false,
        });
        assert_eq!(b.overlay().marks.get(&sq("d4")), Some(&Mark::Place));
        b.apply(Notice::Setup {
            expected,
            diff: vec![],
            turn: Color::White,
            plies: None,
            announce: false,
        });
        assert_eq!(b.overlay().marks.get(&sq("d4")), None);

        b.apply(Notice::Over("Game g over: checkmate — White wins.".into()));
        assert_eq!(b.overlay().hud.banner.as_deref(), Some("Game g over: checkmate — White wins."));
    }
}
