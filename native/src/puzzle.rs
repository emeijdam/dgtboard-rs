//! Solve Lichess puzzles on the board: `dgt puzzle`.
//!
//! A puzzle is a position plus a line of moves. The player sets the position
//! up on the board, plays the solving moves, and executes the opponent's
//! replies as the terminal announces them — a DGT e-board has no lights or
//! display, so the reply is printed here and the referee waits to see it on
//! the board ([`Ruling::ExpectedPlayed`]). Lichess's acceptance rule applies:
//! the solution move, or any move that delivers checkmate.
//!
//! There is no Lichess API for submitting a solve, so this is a trainer: the
//! account's puzzle rating is unaffected.
//!
//! Everything here also works without a board: [`BoardInput::keyboard`] takes
//! moves in UCI from stdin and simulates the lift/place field updates.

use std::collections::VecDeque;
use std::io::{self, BufRead, Write};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::Duration;

use anyhow::{anyhow, bail, Context};

use crate::lichess::{Lichess, PuzzleAndGame};
use crate::{pick_port, Board, Color, DgtBoard, Event, Piece, RefereedGame, Ruling, Square, Status};

/// Which puzzle(s) to fetch.
#[derive(Clone, Debug)]
pub enum PuzzleSource {
    /// Today's daily puzzle.
    Daily,
    /// One puzzle by id.
    Id(String),
    /// Random puzzles, optionally filtered (see [`Lichess::puzzle_next`]).
    Next {
        /// Theme or opening, e.g. `mateIn2`.
        angle: Option<String>,
        /// `easiest` … `hardest`.
        difficulty: Option<String>,
        /// The solver's colour, `white` or `black`.
        color: Option<String>,
    },
}

/// What [`BoardInput::poll`] observed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Poll {
    /// Nothing happened; sleep a little and poll again.
    Idle,
    /// The board changed — look at [`BoardInput::board`].
    Changed,
    /// The user pressed Enter on an empty line.
    Enter,
    /// The user asked to quit (`q`, or stdin closed).
    Quit,
}

/// Read stdin line by line on a background thread.
pub fn stdin_lines() -> Receiver<String> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        for line in io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    rx
}

/// A simulated board driven from the keyboard: UCI moves become the field
/// updates a real board would send (lift, then place), so move detection and
/// refereeing run exactly as with hardware.
pub struct KeyboardBoard {
    board: Board,
    /// Where `setup` jumps to.
    target: Board,
    lines: Receiver<String>,
    /// Field-update frames still to be delivered, one per poll.
    frames: VecDeque<Board>,
}

impl KeyboardBoard {
    /// An empty board reading commands from stdin.
    pub fn new() -> KeyboardBoard {
        KeyboardBoard::with_lines(stdin_lines())
    }

    /// An empty board reading commands from a channel (tests).
    pub fn with_lines(lines: Receiver<String>) -> KeyboardBoard {
        KeyboardBoard {
            board: Board::empty(),
            target: Board::empty(),
            lines,
            frames: VecDeque::new(),
        }
    }

    /// The simulated board.
    pub fn board(&self) -> &Board {
        &self.board
    }

    /// Jump straight to a position (and make it the `setup` target).
    pub fn set_position(&mut self, board: &Board) {
        self.board = board.clone();
        self.target = board.clone();
        self.frames.clear();
    }

    /// Make `board` the position `setup` jumps to, without changing anything.
    pub fn set_target(&mut self, board: &Board) {
        self.target = board.clone();
    }

    /// Deliver the next queued frame, or interpret the next line of input.
    pub fn poll(&mut self) -> Poll {
        if let Some(frame) = self.frames.pop_front() {
            self.board = frame;
            return Poll::Changed;
        }
        let line = match self.lines.try_recv() {
            Ok(line) => line,
            Err(TryRecvError::Empty) => return Poll::Idle,
            Err(TryRecvError::Disconnected) => return Poll::Quit,
        };
        self.handle(line.trim())
    }

    fn handle(&mut self, line: &str) -> Poll {
        match line {
            "" => Poll::Enter,
            "q" | "quit" | "exit" => Poll::Quit,
            "setup" => {
                self.board = self.target.clone();
                Poll::Changed
            }
            _ if line.starts_with("fen ") => match Board::from_fen_placement(&line[4..]) {
                Ok(b) => {
                    self.board = b;
                    Poll::Changed
                }
                Err(e) => {
                    println!("  ? {e}");
                    Poll::Idle
                }
            },
            _ => match apply_uci(&self.board, line) {
                Ok(frames) => {
                    self.frames = frames;
                    self.poll()
                }
                Err(e) => {
                    println!("  ? {e} — type a UCI move (e2e4, e1g1, e7e8q), `setup`, `fen …`, Enter, or q");
                    Poll::Idle
                }
            },
        }
    }
}

impl Default for KeyboardBoard {
    fn default() -> Self {
        KeyboardBoard::new()
    }
}

fn parse_square(s: &str) -> Option<Square> {
    let b = s.as_bytes();
    if b.len() != 2 || !(b'a'..=b'h').contains(&b[0]) || !(b'1'..=b'8').contains(&b[1]) {
        return None;
    }
    Some(Square::at(b[0] - b'a', b[1] - b'0'))
}

/// The board states a human playing `uci` on `board` would pass through, one
/// field update at a time: capture victim lifted, mover lifted, en-passant
/// victim lifted, mover placed (promoted if asked), and for castling the rook
/// after the king. Castling may be spelled `e1g1` or king-takes-rook `e1h1`.
/// Purely geometric: legality is the referee's job.
pub fn apply_uci(board: &Board, uci: &str) -> Result<VecDeque<Board>, String> {
    if uci.len() < 4 || uci.len() > 5 || !uci.is_ascii() {
        return Err(format!("{uci:?} isn't a move"));
    }
    let from = parse_square(&uci[0..2]).ok_or_else(|| format!("bad square {:?}", &uci[0..2]))?;
    let to = parse_square(&uci[2..4]).ok_or_else(|| format!("bad square {:?}", &uci[2..4]))?;
    let piece = board.get(from).ok_or_else(|| format!("no piece on {from}"))?;
    let color = piece.color();
    let mut frames = VecDeque::new();
    let mut cur = board.clone();
    let mut push = |cur: &Board| frames.push_back(cur.clone());

    // Castling: king moves two files, or "takes" its own rook — on its corner
    // square, so a king stepping onto a rook that has already moved (e.g. Kf1
    // with a rook on f1) is reported as the blunder it is, not as castling.
    let own_corner_rook_on_to = board
        .get(to)
        .map(|p| p.is_rook() && p.color() == color && (to.file() == 0 || to.file() == 7))
        .unwrap_or(false);
    if piece.is_king()
        && from.rank() == to.rank()
        && (own_corner_rook_on_to || from.file().abs_diff(to.file()) == 2)
    {
        let kingside = to.file() > from.file();
        let rank = from.rank();
        let (king_to, rook_from, rook_to) = if kingside {
            (Square::at(6, rank), Square::at(7, rank), Square::at(5, rank))
        } else {
            (Square::at(2, rank), Square::at(0, rank), Square::at(3, rank))
        };
        let rook = board.get(rook_from).ok_or_else(|| format!("no rook on {rook_from} to castle with"))?;
        cur.set(from, None);
        push(&cur);
        cur.set(king_to, Some(piece));
        push(&cur);
        cur.set(rook_from, None);
        push(&cur);
        cur.set(rook_to, Some(rook));
        push(&cur);
        return Ok(frames);
    }
    if own_corner_rook_on_to || board.get(to).map(|p| p.color() == color).unwrap_or(false) {
        return Err(format!("{to} holds your own piece"));
    }
    if from == to {
        return Err("a move needs two different squares".into());
    }

    if board.get(to).is_some() {
        cur.set(to, None); // lift the victim
        push(&cur);
    }
    cur.set(from, None); // lift the mover
    push(&cur);
    // En passant: a pawn moving diagonally onto an empty square.
    if piece.is_pawn() && from.file() != to.file() && board.get(to).is_none() {
        let victim = Square::at(to.file(), from.rank());
        if board.get(victim).map(|p| p.is_pawn() && p.color() != color).unwrap_or(false) {
            cur.set(victim, None);
            push(&cur);
        }
    }
    let placed = match uci.as_bytes().get(4) {
        Some(&c) if piece.is_pawn() && to.is_promotion_rank(color) => {
            let fen = if color == Color::White { c.to_ascii_uppercase() } else { c.to_ascii_lowercase() };
            Piece::from_fen_char(fen as char)
                .filter(|p| !p.is_pawn() && !p.is_king())
                .ok_or_else(|| format!("can't promote to {:?}", c as char))?
        }
        Some(&c) => return Err(format!("unexpected promotion piece {:?}", c as char)),
        None => piece,
    };
    cur.set(to, Some(placed));
    push(&cur);
    Ok(frames)
}

/// Where the moves come from: a real DGT board, or the keyboard.
pub enum BoardInput {
    /// A DGT board over serial. Stdin is still read for Enter / `q`.
    Serial {
        /// The connection, already in update mode.
        board: DgtBoard,
        /// Lines typed by the user.
        lines: Receiver<String>,
    },
    /// No hardware: moves typed as UCI.
    Keyboard(KeyboardBoard),
}

impl BoardInput {
    /// Open the board on `port` (auto-detected if `None`), read the position
    /// once and switch to update mode.
    pub fn open_serial(port: Option<String>, flip: bool) -> anyhow::Result<BoardInput> {
        let path = pick_port(port)?;
        let mut board = DgtBoard::open(&path)
            .with_context(|| format!("opening {path}"))?
            .flipped(flip);
        board
            .snapshot(Duration::from_secs(3))
            .context("reading initial board (is the board powered and connected?)")?;
        board.start_updates()?;
        Ok(BoardInput::Serial {
            board,
            lines: stdin_lines(),
        })
    }

    /// A keyboard-driven board.
    pub fn keyboard() -> BoardInput {
        BoardInput::Keyboard(KeyboardBoard::new())
    }

    /// Whether this is the keyboard simulation.
    pub fn is_keyboard(&self) -> bool {
        matches!(self, BoardInput::Keyboard(_))
    }

    /// The current physical (or simulated) position.
    pub fn board(&self) -> &Board {
        match self {
            BoardInput::Serial { board, .. } => board.board(),
            BoardInput::Keyboard(k) => k.board(),
        }
    }

    /// Keyboard: jump to `board` at once. Serial: no-op (the player sets it up).
    pub fn set_position(&mut self, board: &Board) {
        if let BoardInput::Keyboard(k) = self {
            k.set_position(board);
        }
    }

    /// Keyboard: make `board` the `setup` target. Serial: no-op.
    pub fn set_target(&mut self, board: &Board) {
        if let BoardInput::Keyboard(k) = self {
            k.set_target(board);
        }
    }

    /// Non-blocking poll for the next thing that happened.
    pub fn poll(&mut self) -> anyhow::Result<Poll> {
        match self {
            BoardInput::Keyboard(k) => Ok(k.poll()),
            BoardInput::Serial { board, lines } => {
                match lines.try_recv() {
                    Ok(line) if line.trim().is_empty() => return Ok(Poll::Enter),
                    Ok(line) if matches!(line.trim(), "q" | "quit" | "exit") => return Ok(Poll::Quit),
                    Ok(_) => {}
                    Err(TryRecvError::Disconnected) => return Ok(Poll::Quit),
                    Err(TryRecvError::Empty) => {}
                }
                Ok(match board.poll()? {
                    Some(Event::FieldUpdate { .. }) | Some(Event::BoardDump(_)) => Poll::Changed,
                    _ => Poll::Idle,
                })
            }
        }
    }

    /// Block (polling every 20 ms) until something other than `Idle` happens.
    pub fn wait(&mut self) -> anyhow::Result<Poll> {
        loop {
            match self.poll()? {
                Poll::Idle => thread::sleep(Duration::from_millis(20)),
                other => return Ok(other),
            }
        }
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

/// How a puzzle ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// All solving moves played (or a mate found).
    Solved {
        /// Solving moves played.
        moves: usize,
        /// Wrong tries along the way.
        mistakes: usize,
    },
    /// The user quit.
    Quit,
}

fn fetch(lichess: &Lichess, source: &PuzzleSource) -> anyhow::Result<PuzzleAndGame> {
    Ok(match source {
        PuzzleSource::Daily => lichess.puzzle_daily()?,
        PuzzleSource::Id(id) => lichess.puzzle_by_id(id)?,
        PuzzleSource::Next {
            angle,
            difficulty,
            color,
        } => lichess.puzzle_next(angle.as_deref(), difficulty.as_deref(), color.as_deref())?,
    })
}

fn print_diff(game: &RefereedGame, board: &Board) {
    let diff = game.diff(board);
    if diff.is_empty() {
        return;
    }
    let item = |(sq, want, have): &(Square, Option<Piece>, Option<Piece>)| match (want, have) {
        (Some(w), None) => format!("{sq}: place {}", w.glyph()),
        (None, Some(h)) => format!("{sq}: remove {}", h.glyph()),
        (Some(w), Some(h)) => format!("{sq}: {} → {}", h.glyph(), w.glyph()),
        (None, None) => unreachable!("diff only lists differing squares"),
    };
    println!("  squares to fix: {}", diff.iter().map(item).collect::<Vec<_>>().join(", "));
}

/// Fetch puzzles from `source` and solve them on `input` until the user quits.
/// After a daily or by-id puzzle, Enter continues with random ones.
pub fn run(input: &mut BoardInput, lichess: &Lichess, source: PuzzleSource) -> anyhow::Result<()> {
    if input.is_keyboard() {
        println!("Keyboard mode: type moves in UCI (e2e4, e1g1, e7e8q). `setup` resets the pieces, q quits.\n");
    }
    let mut source = source;
    loop {
        let p = fetch(lichess, &source)?;
        match solve(input, &p)? {
            Outcome::Quit => return Ok(()),
            Outcome::Solved { moves, mistakes } => {
                if mistakes == 0 {
                    println!("\nSolved ({moves}/{moves} moves, 0 mistakes)");
                } else {
                    println!("\nSolved with {mistakes} mistake{}", if mistakes == 1 { "" } else { "s" });
                }
            }
        }
        println!("Enter for the next puzzle, q to quit");
        loop {
            match input.wait()? {
                Poll::Enter => break,
                Poll::Quit => return Ok(()),
                Poll::Changed | Poll::Idle => {}
            }
        }
        if !matches!(source, PuzzleSource::Next { .. }) {
            source = PuzzleSource::Next {
                angle: None,
                difficulty: None,
                color: None,
            };
        }
        println!();
    }
}

/// Walk one puzzle on the board. Returns when it's solved or the user quits.
pub fn solve(input: &mut BoardInput, p: &PuzzleAndGame) -> anyhow::Result<Outcome> {
    let (puzzle, src) = (&p.puzzle, &p.game);
    let game = RefereedGame::from_fen(&puzzle.fen).map_err(|e| anyhow!("puzzle {}: {e}", puzzle.id))?;
    input.set_position(&game.expected_board());
    let mut game = game.with_board(input.board());

    // Header.
    println!(
        "Puzzle {}  ·  rating {}  ·  {} plays  ·  {}",
        puzzle.id,
        puzzle.rating,
        puzzle.plays,
        if puzzle.themes.is_empty() { "-".to_string() } else { puzzle.themes.join(", ") }
    );
    if !src.players.is_empty() {
        let named = |color: &str| {
            src.players
                .iter()
                .find(|pl| pl.color == color)
                .map(|pl| match pl.rating {
                    Some(r) => format!("{} ({r})", pl.name),
                    None => pl.name.clone(),
                })
                .unwrap_or_else(|| "?".into())
        };
        let mut from = format!("From {} vs {}", named("white"), named("black"));
        if !src.perf.name.is_empty() {
            from.push_str(&format!(", {}{}", src.perf.name, if src.rated { " (rated)" } else { "" }));
        }
        if !src.id.is_empty() {
            from.push_str(&format!(" — lichess.org/{}#{}", src.id, puzzle.initial_ply));
        }
        println!("{from}");
    }
    if !puzzle.last_move.is_empty() {
        let san = src.pgn.split_whitespace().last().unwrap_or("");
        let shown = if san.is_empty() { puzzle.last_move.clone() } else { format!("{san} ({})", puzzle.last_move) };
        println!("{} just played {shown}.", game.turn().opposite());
    }
    println!();
    print!("{}", game.expected_board());
    println!("FEN: {}\n", puzzle.fen);

    // Setup phase.
    if !game.in_sync() {
        println!("Set up this position on the board.");
        print_diff(&game, input.board());
        loop {
            match input.wait()? {
                Poll::Changed => match game.update(input.board()) {
                    Some(Ruling::BackInSync) => break,
                    _ => print_diff(&game, input.board()),
                },
                Poll::Quit => return Ok(Outcome::Quit),
                Poll::Enter | Poll::Idle => {}
            }
        }
    }
    println!("✓ Position set. {} to move — find the best move.\n", game.turn());

    let mut mistakes = 0;
    let mut moves = 0;
    let solver = game.turn();
    for (i, ply) in puzzle.solution.iter().enumerate() {
        if i % 2 == 0 {
            // The solver's move.
            let saved = game.clone();
            input.set_target(&saved.expected_board());
            let mated;
            loop {
                match input.wait()? {
                    Poll::Quit => return Ok(Outcome::Quit),
                    Poll::Enter | Poll::Idle => continue,
                    Poll::Changed => {}
                }
                match game.update(input.board()) {
                    Some(Ruling::Legal { uci, san, status }) => {
                        let mate = matches!(status, Status::Checkmate { .. });
                        if uci == *ply || mate {
                            println!("  ✓ {san}");
                            announce(status);
                            moves += 1;
                            mated = mate;
                            break;
                        }
                        mistakes += 1;
                        let expected = saved.san(ply).unwrap_or_else(|| ply.clone());
                        println!("  ✗ {san} isn't it (expected {expected}) — put it back");
                        game = saved.clone().with_board(input.board());
                        loop {
                            match input.wait()? {
                                Poll::Quit => return Ok(Outcome::Quit),
                                Poll::Enter | Poll::Idle => {}
                                Poll::Changed => match game.update(input.board()) {
                                    Some(Ruling::BackInSync) => {
                                        println!("  ✓ Back in position — try again.");
                                        break;
                                    }
                                    _ => print_diff(&game, input.board()),
                                },
                            }
                        }
                    }
                    Some(Ruling::Illegal { uci, .. }) => {
                        println!("  ⚠ {uci} is not legal here — put the piece back");
                    }
                    Some(Ruling::BackInSync) => println!("  ✓ Back in position — your move."),
                    Some(Ruling::ExpectedPlayed { .. }) | Some(Ruling::WrongMove { .. }) | None => {}
                }
            }
            if mated {
                break; // any checkmate solves the puzzle, whatever the line says
            }
        } else {
            // The opponent's reply: announce it, wait for it on the board.
            let before = game.clone();
            let san = game.expect_move(ply).map_err(|e| anyhow!("puzzle {}: {e}", puzzle.id))?;
            input.set_target(&game.expected_board());
            let prompt = format!("  ← {} plays {san} — play it on the board …", solver.opposite());
            print!("{prompt}");
            io::stdout().flush()?;
            loop {
                match input.wait()? {
                    Poll::Quit => {
                        println!();
                        return Ok(Outcome::Quit);
                    }
                    Poll::Enter | Poll::Idle => {}
                    Poll::Changed => match game.update(input.board()) {
                        Some(Ruling::ExpectedPlayed { status, .. }) => {
                            println!(" ✓");
                            announce(status);
                            break;
                        }
                        Some(Ruling::WrongMove { got, .. }) => {
                            let got = before.san(&got).unwrap_or(got);
                            println!("\n  ⚠ you played {got}, expected {san} — take it back");
                            print!("{prompt}");
                            io::stdout().flush()?;
                        }
                        _ => {}
                    },
                }
            }
        }
    }
    if moves == 0 {
        bail!("puzzle {} has no moves to play", puzzle.id);
    }
    Ok(Outcome::Solved { moves, mistakes })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MoveTracker;

    /// Feed the frames to a tracker and return the detected move.
    fn detect(board: &Board, uci: &str) -> String {
        let mut t = MoveTracker::with_side(board.clone(), board.get(parse_square(&uci[..2]).unwrap()).unwrap().color());
        let frames = apply_uci(board, uci).unwrap();
        let moves: Vec<_> = frames.iter().filter_map(|f| t.update(f)).collect();
        assert_eq!(moves.len(), 1, "{uci}: {moves:?}");
        moves[0].uci()
    }

    #[test]
    fn keyboard_moves_look_like_board_moves() {
        let start = Board::startpos();
        assert_eq!(detect(&start, "e2e4"), "e2e4");
        let italian = Board::from_fen_placement("r1bqkb1r/pppp1ppp/2n2n2/4p3/2B1P3/5N2/PPPP1PPP/RNBQK2R").unwrap();
        assert_eq!(detect(&italian, "c4f7"), "c4f7"); // capture
        assert_eq!(detect(&italian, "e1g1"), "e1g1"); // castling
        assert_eq!(detect(&italian, "e1h1"), "e1g1"); // king-takes-rook spelling
        let promo = Board::from_fen_placement("8/4P2k/8/8/8/8/8/4K3").unwrap();
        assert_eq!(detect(&promo, "e7e8q"), "e7e8q");
        let after = apply_uci(&promo, "e7e8n").unwrap().back().unwrap().clone();
        assert_eq!(after.get(Square::at(4, 8)), Some(Piece::WhiteKnight));
        let ep = Board::from_fen_placement("4k3/8/8/3pP3/8/8/8/4K3").unwrap();
        assert_eq!(detect(&ep, "e5d6"), "e5d6");
        assert_eq!(apply_uci(&ep, "e5d6").unwrap().back().unwrap().get(Square::at(3, 5)), None);
    }

    #[test]
    fn keyboard_rejects_nonsense() {
        let start = Board::startpos();
        assert!(apply_uci(&start, "e4e5").is_err()); // nothing on e4
        assert!(apply_uci(&start, "e2e2").is_err());
        assert!(apply_uci(&start, "e1e2").is_err()); // own piece on e2
        assert!(apply_uci(&start, "hello").is_err());
        assert!(apply_uci(&start, "e2e4x").is_err());
    }

    #[test]
    fn keyboard_board_delivers_one_frame_per_poll() {
        let (tx, rx) = mpsc::channel();
        let mut kb = KeyboardBoard::with_lines(rx);
        kb.set_position(&Board::startpos());
        assert_eq!(kb.poll(), Poll::Idle);
        tx.send("e2e4".into()).unwrap();
        assert_eq!(kb.poll(), Poll::Changed);
        assert_eq!(kb.board().get(Square::at(4, 2)), None);
        assert_eq!(kb.board().get(Square::at(4, 4)), None);
        assert_eq!(kb.poll(), Poll::Changed);
        assert_eq!(kb.board().get(Square::at(4, 4)), Some(Piece::WhitePawn));
        assert_eq!(kb.poll(), Poll::Idle);
        tx.send("setup".into()).unwrap();
        assert_eq!(kb.poll(), Poll::Changed);
        assert_eq!(kb.board(), &Board::startpos());
        tx.send("".into()).unwrap();
        assert_eq!(kb.poll(), Poll::Enter);
        tx.send("fen 8/8/8/8/8/8/8/8".into()).unwrap();
        assert_eq!(kb.poll(), Poll::Changed);
        assert_eq!(kb.board(), &Board::empty());
        tx.send("q".into()).unwrap();
        assert_eq!(kb.poll(), Poll::Quit);
        drop(tx);
        assert_eq!(kb.poll(), Poll::Quit);
    }

    /// Drive a whole puzzle through the keyboard board, scripted.
    #[test]
    fn solves_a_puzzle_from_the_keyboard_with_a_mistake() {
        let p: PuzzleAndGame = serde_json::from_str(
            r#"{"game":{"pgn":"e4 e5 Nf3 Nc6 Bc4 Nf6","players":[{"name":"a","color":"white","rating":1500},{"name":"b","color":"black"}]},
                "puzzle":{"id":"T1","fen":"r1bqkb1r/pppp1ppp/2n2n2/4p3/2B1P3/5N2/PPPP1PPP/RNBQK2R w KQkq - 4 4",
                "lastMove":"g8f6","solution":["c4f7","e8f7","f3g5"]}}"#,
        )
        .unwrap();
        let (tx, rx) = mpsc::channel();
        for line in ["d2d3", "d3d2", "c4f7", "e8e7", "e7e8", "e8f7", "f3g5"] {
            tx.send(line.into()).unwrap();
        }
        let mut input = BoardInput::Keyboard(KeyboardBoard::with_lines(rx));
        let outcome = solve(&mut input, &p).unwrap();
        assert_eq!(outcome, Outcome::Solved { moves: 2, mistakes: 1 });
    }

    #[test]
    fn any_checkmate_counts() {
        // Lichess accepts any mate: the "solution" here is a different move,
        // but Qxf7# ends the puzzle at once, with no mistake and no further plies.
        let p: PuzzleAndGame = serde_json::from_str(
            r#"{"game":{},"puzzle":{"id":"T2","fen":"r1bqkb1r/pppp1ppp/2n2n2/4p2Q/2B1P3/8/PPPP1PPP/RNB1K1NR w KQkq - 4 4",
                "solution":["c4f7","e8e7","h5e5"]}}"#,
        )
        .unwrap();
        let (tx, rx) = mpsc::channel();
        tx.send("h5f7".into()).unwrap();
        let mut input = BoardInput::Keyboard(KeyboardBoard::with_lines(rx));
        assert_eq!(solve(&mut input, &p).unwrap(), Outcome::Solved { moves: 1, mistakes: 0 });
    }

    #[test]
    fn quitting_mid_puzzle() {
        let p: PuzzleAndGame = serde_json::from_str(
            r#"{"game":{},"puzzle":{"id":"T3","fen":"r1bqkb1r/pppp1ppp/2n2n2/4p3/2B1P3/5N2/PPPP1PPP/RNBQK2R w KQkq - 4 4",
                "solution":["c4f7","e8f7","f3g5"]}}"#,
        )
        .unwrap();
        let (tx, rx) = mpsc::channel();
        tx.send("c4f7".into()).unwrap();
        tx.send("q".into()).unwrap();
        let mut input = BoardInput::Keyboard(KeyboardBoard::with_lines(rx));
        assert_eq!(solve(&mut input, &p).unwrap(), Outcome::Quit);
    }
}
