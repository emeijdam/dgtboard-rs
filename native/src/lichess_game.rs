//! Play Lichess games on the board: `dgt lichess`.
//!
//! The account's event stream says when a game starts; the game's own stream
//! then delivers every move. Moves you play on the board are refereed
//! locally and sent to Lichess; the opponent's moves are printed for you to
//! execute (a DGT e-board has no lights), and the referee waits to see them
//! ([`Ruling::ExpectedPlayed`]). [`RemoteGame`] does the reconciliation; this
//! module is the terminal UX and the transport.
//!
//! Quitting leaves the game running on Lichess — nothing is resigned on your
//! behalf. Type `resign` (or `abort` before the second move) to end it.
//!
//! Works without hardware too: [`BoardInput::keyboard`] takes UCI moves on
//! stdin and the setup phases are satisfied automatically.

use std::collections::HashSet;
use std::fmt;
use std::io::{self, Write};
use std::sync::mpsc::{RecvTimeoutError, TryRecvError};
use std::thread;
use std::time::Duration;

use anyhow::{anyhow, bail, Context};

use crate::lichess::{BoardEvent, GameEvent, GameInfo, GameState, Lichess, LichessError, Seek};
use crate::puzzle::{announce, unknown_line, BoardInput, Poll};
use crate::{Board, Color, IllegalReason, Piece, RefereedGame, RemoteEvent, RemoteGame, Ruling, Square};

/// Where to create a personal API token with the scope the Board API needs.
pub const TOKEN_URL: &str =
    "https://lichess.org/account/oauth/token/create?scopes[]=board:play&description=dgtboard";

/// A clock: minutes per side plus seconds of increment per move.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TimeControl {
    /// Initial time, minutes.
    pub minutes: u32,
    /// Increment, seconds.
    pub increment: u32,
}

impl TimeControl {
    /// Parse `MIN+INC` (e.g. `15+10`, `30+0`); `MIN` alone means no increment.
    pub fn parse(s: &str) -> Result<TimeControl, String> {
        let (m, i) = s.trim().split_once('+').unwrap_or((s.trim(), "0"));
        let minutes = m.parse().map_err(|_| format!("{s:?}: expected MIN+INC, e.g. 15+10"))?;
        let increment = i.parse().map_err(|_| format!("{s:?}: expected MIN+INC, e.g. 15+10"))?;
        Ok(TimeControl { minutes, increment })
    }
}

impl fmt::Display for TimeControl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}+{}", self.minutes, self.increment)
    }
}

/// How to get a game.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Start {
    /// Wait for one started elsewhere (lichess.org, a challenge, the app).
    Wait,
    /// Challenge Stockfish.
    Ai {
        /// Level 1–8.
        level: u8,
        /// Clock.
        time: TimeControl,
    },
    /// Seek a human opponent.
    Seek {
        /// Clock (rapid or slower).
        time: TimeControl,
        /// Rated or casual.
        rated: bool,
    },
}

/// Options for [`run`].
#[derive(Clone, Debug)]
pub struct Options {
    /// How to get a game.
    pub start: Start,
    /// Our colour for `--ai` / `--seek`: `white`, `black` or `random`.
    pub color: String,
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

/// A one-line result for a finished state.
pub fn result_text(state: &GameState) -> String {
    let winner = state.winner.as_deref().map(|w| if w == "white" { "White" } else { "Black" });
    match (state.status.as_str(), winner) {
        ("mate", Some(w)) => format!("checkmate — {w} wins"),
        ("resign", Some(w)) => format!("{w} wins by resignation"),
        ("outoftime" | "timeout", Some(w)) => format!("{w} wins on time"),
        ("stalemate", _) => "stalemate — draw".into(),
        ("draw", _) => "draw".into(),
        ("aborted", _) => "aborted".into(),
        ("noStart", _) => "no start".into(),
        (other, Some(w)) => format!("{w} wins ({other})"),
        (other, None) => other.to_string(),
    }
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

/// Log in, get a game (wait, `--ai`, or `--seek`), play it, repeat. Returns
/// when the user quits, or after the game when one was requested with
/// `--ai` / `--seek`.
pub fn run(input: &mut BoardInput, lichess: &Lichess, opts: Options) -> anyhow::Result<()> {
    if !lichess.is_authenticated() {
        bail!(
            "a Lichess API token with the board:play scope is required.\n  Create one at {TOKEN_URL}\n  then export LICHESS_TOKEN=<token> or pass --token-file <path>."
        );
    }
    let account = lichess
        .account()
        .context("checking the Lichess token (is it valid, with the board:play scope?)")?;
    println!("Logged in as {}.", account.username);
    if input.is_keyboard() {
        println!("Keyboard mode: type moves in UCI (e2e4, e1g1, e7e8q); `setup` puts the pieces where they should be, `resign` resigns, q quits.");
    }

    // The event stream first, so a game created below can't slip past it.
    let mut events = lichess.stream_events().context("opening the Lichess event stream")?;
    let mut queue: Vec<GameInfo> = lichess
        .playing()
        .context("listing games in progress")?
        .now_playing;
    if let Some(g) = queue.first() {
        println!("Rejoining game {} already in progress.", g.game_id);
    }

    let mut seek: Option<Seek> = None;
    match opts.start {
        Start::Wait => {}
        Start::Ai { level, time } => {
            let created = lichess
                .challenge_ai(level, time.minutes * 60, time.increment, &opts.color)
                .context("challenging Stockfish")?;
            println!("Challenged Stockfish level {level} ({time}, you play {}) — game {}.", opts.color, created.id);
        }
        Start::Seek { time, rated } => {
            seek = Some(
                lichess
                    .seek(time.minutes, time.increment, rated, &opts.color)
                    .context("creating the seek")?,
            );
            println!(
                "Seeking a {time} {} game as {} …",
                if rated { "rated" } else { "casual" },
                opts.color
            );
        }
    }
    let one_game = !matches!(opts.start, Start::Wait);
    let mut finished = HashSet::new();

    loop {
        if let Some(info) = queue.pop() {
            if finished.contains(&info.game_id) {
                continue;
            }
            let outcome = play(input, lichess, &info)?;
            finished.insert(info.game_id.clone());
            match outcome {
                Played::Quit => return Ok(()),
                Played::Over if one_game => return Ok(()),
                Played::Over => println!(),
            }
        }
        println!("Waiting for a game… (start one on lichess.org, or use --ai / --seek)");
        loop {
            match events.recv_timeout(Duration::from_millis(50)) {
                Ok(Ok(BoardEvent::GameStart { game })) => {
                    if !finished.contains(&game.game_id) {
                        queue.push(game);
                        break;
                    }
                }
                Ok(Ok(BoardEvent::Challenge { challenge })) => {
                    let from = challenge.challenger.as_ref().map(|c| c.name.as_str()).unwrap_or("someone");
                    println!("Challenge from {from} — accept it on lichess.org to play it here.");
                }
                Ok(Ok(_)) => {}
                Ok(Err(e)) => eprintln!("event stream: {e}"),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    eprintln!("Lichess closed the event stream; reconnecting …");
                    thread::sleep(Duration::from_secs(2));
                    events = lichess.stream_events().context("reopening the Lichess event stream")?;
                }
            }
            if let Some(s) = &seek {
                if let Some(result) = s.finished() {
                    if let Err(e) = result {
                        println!("The seek ended: {e}");
                    }
                    seek = None;
                }
            }
            match input.poll()? {
                Poll::Quit => return Ok(()),
                Poll::Line(l) => unknown_line(&l),
                Poll::Idle | Poll::Changed | Poll::Enter => {}
            }
        }
    }
}

/// How a game session ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Played {
    /// The game is over on Lichess.
    Over,
    /// The user quit; the game is still running on Lichess.
    Quit,
}

/// Drive one game on the board until it ends or the user quits.
pub fn play(input: &mut BoardInput, lichess: &Lichess, info: &GameInfo) -> anyhow::Result<Played> {
    let id = info.game_id.as_str();
    let me = info.my_color();
    let stream = lichess
        .stream_game(id)
        .with_context(|| format!("opening the stream for game {id}"))?;

    // The first record is the whole game.
    let (initial_fen, header, state) = loop {
        match stream.recv_timeout(Duration::from_secs(30)) {
            Ok(Ok(GameEvent::GameFull {
                clock, white, black, initial_fen, state, rated, speed, ..
            })) => {
                let opponent = if me == Color::White { &black } else { &white };
                let mut who = opponent.display_name();
                if let Some(r) = opponent.rating.or(info.opponent.rating) {
                    who.push_str(&format!(" ({r})"));
                }
                let tc = clock.map(|c| c.label()).unwrap_or_else(|| speed.clone());
                let header = format!(
                    "Game {id}: you are {me} vs. {who} · {tc}{}",
                    if rated { " rated" } else { "" }
                );
                break (initial_fen, header, state);
            }
            Ok(Ok(_)) => continue,
            Ok(Err(e)) => bail!("game {id}: {e}"),
            Err(_) => bail!("game {id}: no game data received"),
        }
    };
    println!("{header}");
    let initial = (!initial_fen.is_empty() && initial_fen != "startpos").then_some(initial_fen.as_str());
    let remote = RemoteGame::new(initial, me).map_err(|e| anyhow!("game {id}: {e}"))?;
    input.set_position(&remote.game().expected_board());
    let remote = remote.with_board(input.board());

    let mut session = Session {
        lichess,
        id,
        input,
        remote,
        prompt: None,
        before_expect: None,
        last: GameState::default(),
        over: false,
    };
    if !session.remote.game().in_sync() {
        println!("\n{}", session.remote.game().expected_board());
        println!("Set up this position on the board.");
        print_diff(session.remote.game(), session.input.board());
    }
    session.apply_state(&state)?;
    if session.over {
        return Ok(Played::Over);
    }
    if session.remote.my_turn() && session.remote.game().in_sync() {
        println!("  Your move.");
    }

    loop {
        loop {
            match stream.try_recv() {
                Ok(Ok(event)) => session.on_event(event)?,
                Ok(Err(e)) => eprintln!("game stream: {e}"),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    if !session.over {
                        session.end_prompt();
                        println!("Lichess closed the game stream (game {id}).");
                        return Ok(Played::Over);
                    }
                    break;
                }
            }
        }
        if session.over {
            return Ok(Played::Over);
        }
        match session.input.poll()? {
            Poll::Idle => thread::sleep(Duration::from_millis(20)),
            Poll::Changed => session.on_board()?,
            Poll::Enter => {}
            Poll::Line(line) => session.on_command(&line)?,
            Poll::Quit => {
                session.end_prompt();
                println!("Leaving game {id} running on Lichess (not resigned) — lichess.org/{id}");
                return Ok(Played::Quit);
            }
        }
    }
}

/// One game in progress.
struct Session<'a> {
    lichess: &'a Lichess,
    id: &'a str,
    input: &'a mut BoardInput,
    remote: RemoteGame,
    /// The "play it on the board …" line, while an opponent move is pending.
    prompt: Option<String>,
    /// The game before the pending opponent move, to name a wrong move in SAN.
    before_expect: Option<RefereedGame>,
    /// The previous state, to report offers only when they appear.
    last: GameState,
    over: bool,
}

impl Session<'_> {
    /// Finish a pending prompt line before printing something else.
    fn end_prompt(&mut self) {
        if self.prompt.take().is_some() {
            println!();
        }
    }

    fn show_prompt(&self) {
        if let Some(p) = &self.prompt {
            print!("{p}");
            let _ = io::stdout().flush();
        }
    }

    fn opponent(&self) -> Color {
        self.remote.me().opposite()
    }

    fn on_event(&mut self, event: GameEvent) -> anyhow::Result<()> {
        match event {
            GameEvent::GameState(state) => self.apply_state(&state)?,
            GameEvent::ChatLine { username, text, room } if room != "spectator" => {
                self.end_prompt();
                println!("  💬 {username}: {text}");
                self.show_prompt();
            }
            GameEvent::OpponentGone { gone: true, claim_win_in_seconds } => {
                self.end_prompt();
                match claim_win_in_seconds {
                    Some(s) => println!("  Opponent has left the game — you can claim the win in {s} s on lichess.org/{}", self.id),
                    None => println!("  Opponent has left the game."),
                }
                self.show_prompt();
            }
            GameEvent::OpponentGone { gone: false, .. } => {
                self.end_prompt();
                println!("  Opponent is back.");
                self.show_prompt();
            }
            GameEvent::GameFull { .. } | GameEvent::ChatLine { .. } | GameEvent::Other => {}
        }
        Ok(())
    }

    /// A new state from Lichess: clocks, offers, moves, result.
    fn apply_state(&mut self, state: &GameState) -> anyhow::Result<()> {
        self.end_prompt();
        println!("  ⏱ {} / {}", clock_text(state.wtime), clock_text(state.btime));

        let opp = self.opponent();
        let (draw, last_draw) = match opp {
            Color::White => (state.wdraw, self.last.wdraw),
            Color::Black => (state.bdraw, self.last.bdraw),
        };
        let (takeback, last_takeback) = match opp {
            Color::White => (state.wtakeback, self.last.wtakeback),
            Color::Black => (state.btakeback, self.last.btakeback),
        };
        if draw && !last_draw {
            println!("  {opp} offers a draw — answer on lichess.org/{}", self.id);
        }
        if takeback && !last_takeback {
            println!("  {opp} proposes a takeback — answer on lichess.org/{}", self.id);
        }
        self.last = state.clone();

        let before = self.remote.game().clone();
        match self.remote.sync(&state.moves).map_err(|e| anyhow!("game {}: {e}", self.id))? {
            None => {}
            Some(RemoteEvent::OpponentMoved { san, .. }) => {
                self.before_expect = Some(before);
                self.input.set_target(&self.remote.game().expected_board());
                if state.is_ongoing() {
                    self.prompt = Some(format!("  ← {opp} plays {san} — play it on the board …"));
                    self.show_prompt();
                } else {
                    println!("  ← {opp} plays {san}");
                }
            }
            Some(RemoteEvent::Contradicted { ply, sent, server }) => {
                println!("  ⚠ Lichess has {server} at ply {ply}, not the {sent} sent from here — fix the board");
                self.resync()?;
            }
            Some(RemoteEvent::Replaced { plies }) => {
                println!("\n{}", self.remote.game().expected_board());
                println!(
                    "Set up this position on the board ({plies} plies played, {} to move).",
                    self.remote.game().turn()
                );
                self.resync()?;
            }
        }

        if !state.is_ongoing() {
            self.end_prompt();
            println!("\nGame {} over: {}.", self.id, result_text(state));
            self.over = true;
        }
        Ok(())
    }

    /// The position changed under the board: show what to fix (the keyboard
    /// board jumps there at once).
    fn resync(&mut self) -> anyhow::Result<()> {
        let expected = self.remote.game().expected_board();
        self.input.set_target(&expected);
        if self.input.is_keyboard() {
            self.input.set_position(&expected);
            self.on_board()
        } else {
            print_diff(self.remote.game(), self.input.board());
            Ok(())
        }
    }

    /// The board changed.
    fn on_board(&mut self) -> anyhow::Result<()> {
        match self.remote.update(self.input.board()) {
            Some(Ruling::Legal { uci, san, status }) => match self.lichess.make_move(self.id, &uci) {
                Ok(()) => {
                    println!("  → {san} sent");
                    announce(status);
                }
                Err(e) => {
                    self.remote.rejected();
                    self.input.set_target(&self.remote.game().expected_board());
                    match e {
                        LichessError::Refused(reason) => println!("  ✗ Lichess refused {san}: {reason} — put it back"),
                        other => println!("  ✗ Sending {san} failed: {other} — put it back and try again"),
                    }
                    print_diff(self.remote.game(), self.input.board());
                }
            },
            Some(Ruling::Illegal { uci, reason, piece }) => {
                println!(
                    "  ⚠ {uci} is not legal here — {}. Put the {} back.",
                    reason_text(reason),
                    piece.glyph()
                );
            }
            Some(Ruling::BackInSync) => {
                if self.remote.my_turn() {
                    println!("  ✓ Board in sync — your move.");
                } else {
                    println!("  ✓ Board in sync.");
                }
            }
            Some(Ruling::ExpectedPlayed { status, .. }) => {
                self.prompt = None;
                self.before_expect = None;
                println!(" ✓");
                announce(status);
            }
            Some(Ruling::WrongMove { expected, got }) => {
                let before = self.before_expect.as_ref();
                let got = before.and_then(|g| g.san(&got)).unwrap_or(got);
                let expected = before.and_then(|g| g.san(&expected)).unwrap_or(expected);
                println!("\n  ⚠ you played {got}, expected {expected} — take it back");
                self.show_prompt();
            }
            None => {
                if !self.remote.game().in_sync() && self.remote.game().expected_move().is_none() {
                    print_diff(self.remote.game(), self.input.board());
                }
            }
        }
        Ok(())
    }

    /// A typed command.
    fn on_command(&mut self, line: &str) -> anyhow::Result<()> {
        self.end_prompt();
        match line {
            "resign" => match self.lichess.resign(self.id) {
                Ok(()) => println!("  Resigned."),
                Err(e) => println!("  ✗ Resign failed: {e}"),
            },
            "abort" => match self.lichess.abort(self.id) {
                Ok(()) => println!("  Aborted."),
                Err(e) => println!("  ✗ Abort failed: {e}"),
            },
            "board" => {
                println!("{}", self.remote.game().expected_board());
                print_diff(self.remote.game(), self.input.board());
            }
            _ => {
                unknown_line(line);
                println!("    In a game: `resign`, `abort`, `board`.");
            }
        }
        self.show_prompt();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::puzzle::KeyboardBoard;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::mpsc::{self, Receiver, Sender};
    use std::sync::{Arc, Mutex};

    /// A tiny in-process Lichess: one account, one game against "Stockfish",
    /// enough of the Board API for a whole `run` to go through. Request
    /// paths are recorded; game states are pushed on the game stream when a
    /// move or a resignation is posted.
    struct FakeLichess {
        base: String,
        requests: Arc<Mutex<Vec<String>>>,
        /// Fires once the AI's reply has been written to the game stream.
        replied: Receiver<()>,
        /// Held so the open streams stay open until the test ends.
        _keep: Sender<()>,
    }

    fn fake_lichess() -> FakeLichess {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let (states_tx, states_rx) = mpsc::channel::<String>();
        let states_rx = Arc::new(Mutex::new(Some(states_rx)));
        let (replied_tx, replied) = mpsc::channel();
        let (keep, keep_rx) = mpsc::channel::<()>();
        let keep_rx = Arc::new(Mutex::new(keep_rx));
        let seen = Arc::clone(&requests);
        thread::spawn(move || {
            for stream in listener.incoming() {
                let mut stream = stream.unwrap();
                let (head, _body) = read_request(&mut stream);
                let line = head.lines().next().unwrap_or("").to_string();
                seen.lock().unwrap().push(line.clone());
                let states_tx = states_tx.clone();
                let states_rx = Arc::clone(&states_rx);
                let replied_tx = replied_tx.clone();
                let keep_rx = Arc::clone(&keep_rx);
                thread::spawn(move || {
                    let path = line.split(' ').nth(1).unwrap_or("");
                    let method = line.split(' ').next().unwrap_or("");
                    match (method, path) {
                        ("GET", "/api/account") => respond(&mut stream, "200 OK", r#"{"id":"me","username":"Me"}"#),
                        ("GET", "/api/account/playing") => respond(&mut stream, "200 OK", r#"{"nowPlaying":[]}"#),
                        ("POST", "/api/challenge/ai") => respond(&mut stream, "201 Created", r#"{"id":"fake0001"}"#),
                        ("GET", "/api/stream/event") => {
                            start_stream(&mut stream);
                            writeln!(stream, r#"{{"type":"gameStart","game":{{"gameId":"fake0001","fullId":"fake0001abcd","color":"white","fen":"startpos","isMyTurn":true,"opponent":{{"id":"ai","username":"Stockfish level 1","ai":1}}}}}}"#).unwrap();
                            stream.flush().unwrap();
                            let _ = keep_rx.lock().unwrap().recv(); // stay open
                        }
                        ("GET", "/api/board/game/stream/fake0001") => {
                            start_stream(&mut stream);
                            writeln!(stream, r#"{{"type":"gameFull","id":"fake0001","rated":false,"clock":{{"initial":900000,"increment":10000}},"speed":"rapid","white":{{"id":"me","name":"Me","rating":1500}},"black":{{"aiLevel":1}},"initialFen":"startpos","state":{{"type":"gameState","moves":"","wtime":900000,"btime":900000,"status":"started"}}}}"#).unwrap();
                            stream.flush().unwrap();
                            let rx = states_rx.lock().unwrap().take().expect("one game stream");
                            for state in rx {
                                writeln!(stream, "{state}").unwrap();
                                stream.flush().unwrap();
                                if state.contains("e7e5") {
                                    let _ = replied_tx.send(());
                                }
                            }
                        }
                        ("POST", "/api/board/game/fake0001/move/e2e4") => {
                            respond(&mut stream, "200 OK", r#"{"ok":true}"#);
                            states_tx.send(r#"{"type":"gameState","moves":"e2e4","wtime":900000,"btime":900000,"status":"started"}"#.into()).unwrap();
                            states_tx.send(r#"{"type":"gameState","moves":"e2e4 e7e5","wtime":900000,"btime":892000,"status":"started","bdraw":true}"#.into()).unwrap();
                        }
                        ("POST", "/api/board/game/fake0001/move/d7d5") => {
                            respond(&mut stream, "400 Bad Request", r#"{"error":"Not your turn, or game already over"}"#)
                        }
                        ("POST", "/api/board/game/fake0001/resign") => {
                            respond(&mut stream, "200 OK", r#"{"ok":true}"#);
                            states_tx.send(r#"{"type":"gameState","moves":"e2e4 e7e5","wtime":880000,"btime":892000,"status":"resign","winner":"black"}"#.into()).unwrap();
                        }
                        _ => respond(&mut stream, "404 Not Found", r#"{"error":"nope"}"#),
                    }
                });
            }
        });
        FakeLichess {
            base,
            requests,
            replied,
            _keep: keep,
        }
    }

    fn read_request(stream: &mut TcpStream) -> (String, Vec<u8>) {
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut head = String::new();
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            if line.trim_end().is_empty() {
                break;
            }
            head.push_str(&line);
        }
        let length: usize = head
            .lines()
            .find_map(|l| l.strip_prefix("Content-Length: "))
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(0);
        let mut body = vec![0u8; length];
        reader.read_exact(&mut body).unwrap();
        (head, body)
    }

    fn respond(stream: &mut TcpStream, status: &str, body: &str) {
        write!(
            stream,
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
        stream.flush().unwrap();
    }

    fn start_stream(stream: &mut TcpStream) {
        write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/x-ndjson\r\nConnection: close\r\n\r\n").unwrap();
        stream.flush().unwrap();
    }

    /// The whole `--ai 1 --color white --keyboard` flow against the fake:
    /// e4 is sent, the reply e5 is announced, a wrong move is flagged, the
    /// reply is played, and `resign` ends the game.
    #[test]
    fn plays_a_game_from_the_keyboard_against_a_fake_lichess() {
        let fake = fake_lichess();
        let lichess = Lichess::with_token("lip_test").with_base_url(fake.base.clone());
        let (tx, rx) = mpsc::channel();
        tx.send("e2e4".into()).unwrap();
        let replied = fake.replied;
        thread::spawn(move || {
            replied.recv().unwrap();
            thread::sleep(Duration::from_millis(300)); // let the client read the reply
            for line in ["d7d5", "d5d7", "e7e5", "resign"] {
                tx.send(line.into()).unwrap();
                thread::sleep(Duration::from_millis(50));
            }
        });
        let mut input = BoardInput::Keyboard(KeyboardBoard::with_lines(rx));
        let opts = Options {
            start: Start::Ai {
                level: 1,
                time: TimeControl::parse("15+10").unwrap(),
            },
            color: "white".into(),
        };
        run(&mut input, &lichess, opts).unwrap();

        let requests = fake.requests.lock().unwrap().clone();
        let posted: Vec<_> = requests.iter().filter(|r| r.starts_with("POST")).cloned().collect();
        assert_eq!(
            posted,
            [
                "POST /api/challenge/ai HTTP/1.1",
                "POST /api/board/game/fake0001/move/e2e4 HTTP/1.1",
                "POST /api/board/game/fake0001/resign HTTP/1.1",
            ],
            "{requests:?}"
        );
        assert!(requests.iter().any(|r| r.starts_with("GET /api/stream/event ")), "{requests:?}");
        assert!(requests.iter().any(|r| r.starts_with("GET /api/board/game/stream/fake0001 ")), "{requests:?}");
    }

    #[test]
    fn parses_time_controls() {
        assert_eq!(TimeControl::parse("15+10").unwrap(), TimeControl { minutes: 15, increment: 10 });
        assert_eq!(TimeControl::parse(" 30 ").unwrap(), TimeControl { minutes: 30, increment: 0 });
        assert_eq!(TimeControl::parse("15+10").unwrap().to_string(), "15+10");
        assert!(TimeControl::parse("fast").is_err());
        assert!(TimeControl::parse("15+").is_err());
    }

    #[test]
    fn formats_clocks_and_results() {
        assert_eq!(clock_text(892_000), "14:52");
        assert_eq!(clock_text(0), "00:00");
        assert_eq!(clock_text(3_661_000), "1:01:01");
        let state = |status: &str, winner: Option<&str>| GameState {
            status: status.into(),
            winner: winner.map(str::to_string),
            ..GameState::default()
        };
        assert_eq!(result_text(&state("mate", Some("white"))), "checkmate — White wins");
        assert_eq!(result_text(&state("resign", Some("black"))), "Black wins by resignation");
        assert_eq!(result_text(&state("outoftime", Some("white"))), "White wins on time");
        assert_eq!(result_text(&state("draw", None)), "draw");
        assert_eq!(result_text(&state("aborted", None)), "aborted");
        assert!(state("started", None).is_ongoing());
        assert!(!state("mate", Some("white")).is_ongoing());
    }
}
