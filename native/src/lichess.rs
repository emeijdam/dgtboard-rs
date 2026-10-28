//! A small Lichess API client: puzzles, and the Board API for playing games.
//!
//! Only what the `dgt` CLI needs is modelled. Requests are plain blocking HTTP
//! ([`ureq`]); a personal API token is optional for puzzles (with one Lichess
//! serves puzzles the account hasn't seen) and required for the Board API
//! (scope `board:play`). The token is never logged or printed.
//!
//! The Board API is event driven: [`Lichess::stream_events`] delivers
//! [`BoardEvent`]s for the account (a game starting, a challenge), and
//! [`Lichess::stream_game`] delivers [`GameEvent`]s for one game, starting
//! with the full game and then one [`GameState`] per move; both are ndjson
//! streams read on a background thread. Unknown record types and fields are
//! ignored.
//!
//! Lichess asks for one request at a time and answers `429 Too Many Requests`
//! when that's exceeded; the client then waits a minute and retries once.

use std::io::{self, BufRead, BufReader, Read};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde::Deserialize;

use crate::Color;

/// Errors from talking to Lichess.
#[derive(Debug, thiserror::Error)]
pub enum LichessError {
    /// The HTTP request failed or Lichess answered with an error status.
    #[error("lichess: {0}")]
    Http(Box<ureq::Error>),
    /// Reading the response body failed.
    #[error("lichess: reading response: {0}")]
    Io(#[from] io::Error),
    /// The response body wasn't the JSON we expected.
    #[error("lichess: unexpected response: {0}")]
    Json(#[from] serde_json::Error),
    /// Rate limited twice in a row.
    #[error("lichess: rate limited (HTTP 429) — try again in a minute")]
    RateLimited,
    /// Lichess refused the request (HTTP 400): an illegal move, not your
    /// turn, a game that's over, or bad parameters. Carries Lichess's reason.
    #[error("lichess refused: {0}")]
    Refused(String),
}

impl From<ureq::Error> for LichessError {
    fn from(e: ureq::Error) -> Self {
        LichessError::Http(Box::new(e))
    }
}

/// Convenience alias.
pub type Result<T> = std::result::Result<T, LichessError>;

/// A puzzle with the game it was taken from, as returned by
/// `/api/puzzle/daily`, `/api/puzzle/{id}` and `/api/puzzle/next`.
#[derive(Clone, Debug, Deserialize)]
pub struct PuzzleAndGame {
    /// The source game.
    pub game: PuzzleGame,
    /// The puzzle itself.
    pub puzzle: Puzzle,
}

/// The game a puzzle was taken from.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct PuzzleGame {
    /// Lichess game id.
    #[serde(default)]
    pub id: String,
    /// Time-control category (bullet, blitz, rapid, …).
    #[serde(default)]
    pub perf: Perf,
    /// Whether the game was rated.
    #[serde(default)]
    pub rated: bool,
    /// The two players, White first.
    #[serde(default)]
    pub players: Vec<Player>,
    /// The moves up to the puzzle position, as space-separated SAN.
    #[serde(default)]
    pub pgn: String,
    /// The time control, e.g. `5+0`.
    #[serde(default)]
    pub clock: Option<String>,
}

/// A rating category.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct Perf {
    /// Machine name, e.g. `blitz`.
    #[serde(default)]
    pub key: String,
    /// Display name, e.g. `Blitz`.
    #[serde(default)]
    pub name: String,
}

/// A player of the source game.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct Player {
    /// Display name (may carry a title prefix).
    #[serde(default)]
    pub name: String,
    /// User id.
    #[serde(default)]
    pub id: Option<String>,
    /// Rating at the time of the game.
    #[serde(default)]
    pub rating: Option<u32>,
    /// `white` or `black`.
    #[serde(default)]
    pub color: String,
}

/// A Lichess puzzle.
#[derive(Clone, Debug, Deserialize)]
pub struct Puzzle {
    /// Puzzle id.
    pub id: String,
    /// Puzzle rating.
    #[serde(default)]
    pub rating: u32,
    /// How often it has been played.
    #[serde(default)]
    pub plays: u64,
    /// Theme tags, e.g. `mateIn2`, `fork`.
    #[serde(default)]
    pub themes: Vec<String>,
    /// The position to solve (after `last_move`), as a full FEN.
    pub fen: String,
    /// The opponent's last move (UCI) that led to `fen`.
    #[serde(default, rename = "lastMove")]
    pub last_move: String,
    /// The solution in UCI plies: the solver's move, the opponent's reply, …
    pub solution: Vec<String>,
    /// Ply number of `fen` in the source game.
    #[serde(default, rename = "initialPly")]
    pub initial_ply: u32,
}

/// The authenticated account (`GET /api/account`).
#[derive(Clone, Debug, Default, Deserialize)]
pub struct Account {
    /// User id (lowercase).
    #[serde(default)]
    pub id: String,
    /// Display name.
    #[serde(default)]
    pub username: String,
}

/// The opponent in a [`GameInfo`].
#[derive(Clone, Debug, Default, Deserialize)]
pub struct Opponent {
    /// User id, or e.g. `ai` for Stockfish.
    #[serde(default)]
    pub id: Option<String>,
    /// Display name (e.g. `Stockfish level 1`).
    #[serde(default)]
    pub username: String,
    /// Rating, if any.
    #[serde(default)]
    pub rating: Option<u32>,
    /// Stockfish level, when the opponent is the computer.
    #[serde(default)]
    pub ai: Option<u8>,
}

/// A game as the account event stream and `/api/account/playing` describe it.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct GameInfo {
    /// Game id (8 characters).
    #[serde(default, rename = "gameId")]
    pub game_id: String,
    /// Game id plus the player's secret suffix.
    #[serde(default, rename = "fullId")]
    pub full_id: String,
    /// Our colour: `white` or `black`.
    #[serde(default)]
    pub color: String,
    /// Current position.
    #[serde(default)]
    pub fen: String,
    /// Whether it's our move.
    #[serde(default, rename = "isMyTurn")]
    pub is_my_turn: bool,
    /// The last move (UCI), if any.
    #[serde(default, rename = "lastMove")]
    pub last_move: String,
    /// Speed category: `rapid`, `classical`, `correspondence`, …
    #[serde(default)]
    pub speed: String,
    /// Whether the game is rated.
    #[serde(default)]
    pub rated: bool,
    /// The other player.
    #[serde(default)]
    pub opponent: Opponent,
}

impl GameInfo {
    /// Our colour as a [`Color`], defaulting to White if unknown.
    pub fn my_color(&self) -> Color {
        if self.color.eq_ignore_ascii_case("black") {
            Color::Black
        } else {
            Color::White
        }
    }
}

/// Games in progress for the account (`GET /api/account/playing`).
#[derive(Clone, Debug, Default, Deserialize)]
pub struct Playing {
    /// The games, most urgent first.
    #[serde(default, rename = "nowPlaying")]
    pub now_playing: Vec<GameInfo>,
}

/// An incoming challenge, as far as we show it.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct Challenge {
    /// Challenge id.
    #[serde(default)]
    pub id: String,
    /// Who sent it.
    #[serde(default)]
    pub challenger: Option<ChallengeUser>,
}

/// A user in a challenge.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct ChallengeUser {
    /// Display name.
    #[serde(default)]
    pub name: String,
    /// Rating.
    #[serde(default)]
    pub rating: Option<u32>,
}

/// A record on the account event stream (`GET /api/stream/event`).
#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "type")]
pub enum BoardEvent {
    /// A game started (or was in progress when the stream opened).
    #[serde(rename = "gameStart")]
    GameStart {
        /// The game.
        game: GameInfo,
    },
    /// A game ended.
    #[serde(rename = "gameFinish")]
    GameFinish {
        /// The game.
        game: GameInfo,
    },
    /// Someone challenged the account.
    #[serde(rename = "challenge")]
    Challenge {
        /// The challenge.
        challenge: Challenge,
    },
    /// Anything else (`challengeCanceled`, `challengeDeclined`, …).
    #[serde(other)]
    Other,
}

/// A player in a [`GameEvent::GameFull`].
#[derive(Clone, Debug, Default, Deserialize)]
pub struct GamePlayer {
    /// User id; absent for the computer.
    #[serde(default)]
    pub id: Option<String>,
    /// Display name; absent for the computer.
    #[serde(default)]
    pub name: Option<String>,
    /// Rating.
    #[serde(default)]
    pub rating: Option<u32>,
    /// Stockfish level, when this player is the computer.
    #[serde(default, rename = "aiLevel")]
    pub ai_level: Option<u8>,
}

impl GamePlayer {
    /// A name to print: the username, or `Stockfish level N`.
    pub fn display_name(&self) -> String {
        match (&self.name, self.ai_level) {
            (Some(n), _) => n.clone(),
            (None, Some(l)) => format!("Stockfish level {l}"),
            (None, None) => "?".into(),
        }
    }
}

/// A game's clock settings, in milliseconds.
#[derive(Clone, Copy, Debug, Default, Deserialize)]
pub struct Clock {
    /// Initial time per player.
    #[serde(default)]
    pub initial: u64,
    /// Increment per move.
    #[serde(default)]
    pub increment: u64,
}

impl Clock {
    /// `15+10` style.
    pub fn label(&self) -> String {
        format!("{}+{}", self.initial / 60_000, self.increment / 1000)
    }
}

/// The live state of a game: all moves so far, clocks, and status.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct GameState {
    /// All moves from the initial position, space-separated UCI.
    #[serde(default)]
    pub moves: String,
    /// White's remaining time, ms.
    #[serde(default)]
    pub wtime: u64,
    /// Black's remaining time, ms.
    #[serde(default)]
    pub btime: u64,
    /// Game status: `started`, `mate`, `resign`, `draw`, `outoftime`, `aborted`, …
    #[serde(default)]
    pub status: String,
    /// The winner (`white` / `black`) once decided.
    #[serde(default)]
    pub winner: Option<String>,
    /// White is offering a draw.
    #[serde(default)]
    pub wdraw: bool,
    /// Black is offering a draw.
    #[serde(default)]
    pub bdraw: bool,
    /// White proposes a takeback.
    #[serde(default)]
    pub wtakeback: bool,
    /// Black proposes a takeback.
    #[serde(default)]
    pub btakeback: bool,
}

impl GameState {
    /// Whether the game is still being played.
    pub fn is_ongoing(&self) -> bool {
        matches!(self.status.as_str(), "" | "created" | "started")
    }
}

/// A record on a game stream (`GET /api/board/game/stream/{id}`).
// `GameFull` is far bigger than the others, but there is exactly one per
// game, so boxing it would buy nothing.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "type")]
pub enum GameEvent {
    /// The first record: the whole game.
    #[serde(rename = "gameFull")]
    GameFull {
        /// Game id.
        #[serde(default)]
        id: String,
        /// Whether the game is rated.
        #[serde(default)]
        rated: bool,
        /// Clock settings; absent for correspondence.
        #[serde(default)]
        clock: Option<Clock>,
        /// Speed category.
        #[serde(default)]
        speed: String,
        /// White.
        #[serde(default)]
        white: GamePlayer,
        /// Black.
        #[serde(default)]
        black: GamePlayer,
        /// `startpos` or a FEN.
        #[serde(default, rename = "initialFen")]
        initial_fen: String,
        /// The state so far.
        #[serde(default)]
        state: GameState,
    },
    /// A new state: a move was played, a draw offered, the game ended.
    #[serde(rename = "gameState")]
    GameState(GameState),
    /// A chat message.
    #[serde(rename = "chatLine")]
    ChatLine {
        /// Who wrote it.
        #[serde(default)]
        username: String,
        /// The message.
        #[serde(default)]
        text: String,
        /// `player` or `spectator`.
        #[serde(default)]
        room: String,
    },
    /// The opponent left (or came back).
    #[serde(rename = "opponentGone")]
    OpponentGone {
        /// Whether they are gone right now.
        #[serde(default)]
        gone: bool,
        /// Seconds until a win can be claimed, if they stay away.
        #[serde(default, rename = "claimWinInSeconds")]
        claim_win_in_seconds: Option<u64>,
    },
    /// Anything else.
    #[serde(other)]
    Other,
}

/// A newly created game (`POST /api/challenge/ai`).
#[derive(Clone, Debug, Default, Deserialize)]
pub struct CreatedGame {
    /// Game id.
    #[serde(default)]
    pub id: String,
}

/// An open seek: Lichess keeps looking for an opponent as long as the
/// request's connection stays open, so this holds it on a background thread.
/// Dropping the handle does **not** cancel the seek (the thread keeps the
/// connection); it ends when Lichess closes it — once a game is found, or
/// when the process exits.
pub struct Seek {
    done: Receiver<Result<()>>,
}

impl Seek {
    /// Whether the seek has ended, and how: `Ok` when Lichess closed it
    /// normally (a game was found), `Err` if the connection failed.
    pub fn finished(&self) -> Option<Result<()>> {
        self.done.try_recv().ok()
    }
}

/// The Lichess API, optionally authenticated.
pub struct Lichess {
    token: Option<String>,
    base: String,
}

impl Default for Lichess {
    fn default() -> Self {
        Lichess::anonymous()
    }
}

impl Lichess {
    /// Public endpoints only.
    pub fn anonymous() -> Lichess {
        Lichess {
            token: None,
            base: "https://lichess.org".into(),
        }
    }

    /// Authenticate with a personal API token.
    pub fn with_token(token: impl Into<String>) -> Lichess {
        let token = token.into();
        Lichess {
            token: (!token.trim().is_empty()).then(|| token.trim().to_string()),
            ..Lichess::anonymous()
        }
    }

    /// Use the `LICHESS_TOKEN` environment variable if set, else anonymous.
    pub fn from_env() -> Lichess {
        match std::env::var("LICHESS_TOKEN") {
            Ok(t) => Lichess::with_token(t),
            Err(_) => Lichess::anonymous(),
        }
    }

    /// Read the token from a file (first line, trimmed).
    pub fn with_token_file(path: impl AsRef<std::path::Path>) -> io::Result<Lichess> {
        let text = std::fs::read_to_string(path)?;
        Ok(Lichess::with_token(text.lines().next().unwrap_or("")))
    }

    /// Point the client at another server (tests, mirrors).
    pub fn with_base_url(mut self, base: impl Into<String>) -> Lichess {
        self.base = base.into();
        self.base.truncate(self.base.trim_end_matches('/').len());
        self
    }

    /// Whether requests carry a token.
    pub fn is_authenticated(&self) -> bool {
        self.token.is_some()
    }

    /// Today's daily puzzle.
    pub fn puzzle_daily(&self) -> Result<PuzzleAndGame> {
        self.get_json("/api/puzzle/daily")
    }

    /// A puzzle by id.
    pub fn puzzle_by_id(&self, id: &str) -> Result<PuzzleAndGame> {
        self.get_json(&format!("/api/puzzle/{id}"))
    }

    /// A random puzzle, optionally filtered by theme (`angle`, e.g. `mateIn2`
    /// or an opening), `difficulty` (`easiest`, `easier`, `normal`, `harder`,
    /// `hardest`) and the solver's `color` (`white` / `black`). With a token
    /// Lichess avoids puzzles the account has already played.
    pub fn puzzle_next(
        &self,
        angle: Option<&str>,
        difficulty: Option<&str>,
        color: Option<&str>,
    ) -> Result<PuzzleAndGame> {
        let mut query = Vec::new();
        for (key, value) in [("angle", angle), ("difficulty", difficulty), ("color", color)] {
            if let Some(v) = value {
                query.push(format!("{key}={v}"));
            }
        }
        let path = if query.is_empty() {
            "/api/puzzle/next".to_string()
        } else {
            format!("/api/puzzle/next?{}", query.join("&"))
        };
        self.get_json(&path)
    }

    /// The authenticated account. Fails without a token (or with one that
    /// has been revoked).
    pub fn account(&self) -> Result<Account> {
        self.get_json("/api/account")
    }

    /// Games the account is playing right now.
    pub fn playing(&self) -> Result<Playing> {
        self.get_json("/api/account/playing")
    }

    /// Open the account's event stream. One per token; a blank keep-alive
    /// arrives every few seconds (skipped here). The receiver ends when
    /// Lichess closes the stream.
    pub fn stream_events(&self) -> Result<Receiver<Result<BoardEvent>>> {
        self.stream("/api/stream/event")
    }

    /// Open a game's stream: first [`GameEvent::GameFull`], then a
    /// [`GameEvent::GameState`] per move; it closes when the game ends.
    pub fn stream_game(&self, game_id: &str) -> Result<Receiver<Result<GameEvent>>> {
        self.stream(&format!("/api/board/game/stream/{game_id}"))
    }

    /// Play `uci` in the game. [`LichessError::Refused`] if it's illegal, not
    /// our turn, or the game is over.
    pub fn make_move(&self, game_id: &str, uci: &str) -> Result<()> {
        self.post(&format!("/api/board/game/{game_id}/move/{uci}"), &[])
            .map(drop)
    }

    /// Resign the game.
    pub fn resign(&self, game_id: &str) -> Result<()> {
        self.post(&format!("/api/board/game/{game_id}/resign"), &[]).map(drop)
    }

    /// Abort the game (only before both sides have moved).
    pub fn abort(&self, game_id: &str) -> Result<()> {
        self.post(&format!("/api/board/game/{game_id}/abort"), &[]).map(drop)
    }

    /// Start a game against Stockfish: `level` 1–8, clock `limit_s` seconds
    /// plus `inc_s` per move, `color` `white` / `black` / `random`. The game
    /// then arrives on the event stream as [`BoardEvent::GameStart`].
    pub fn challenge_ai(&self, level: u8, limit_s: u32, inc_s: u32, color: &str) -> Result<CreatedGame> {
        let (level, limit, inc) = (level.to_string(), limit_s.to_string(), inc_s.to_string());
        let text = self
            .post(
                "/api/challenge/ai",
                &[("level", &level), ("clock.limit", &limit), ("clock.increment", &inc), ("color", color)],
            )?
            .into_string()?;
        Ok(serde_json::from_str(&text)?)
    }

    /// Look for a human opponent: `time_min` minutes plus `inc_s` seconds per
    /// move (rapid or slower — Lichess only pairs blitz through challenges),
    /// `rated` or casual, `color` `white` / `black` / `random`. The seek
    /// lives as long as the returned [`Seek`]'s connection; the game it finds
    /// arrives on the event stream.
    pub fn seek(&self, time_min: u32, inc_s: u32, rated: bool, color: &str) -> Result<Seek> {
        let (time, inc) = (time_min.to_string(), inc_s.to_string());
        let response = self.post(
            "/api/board/seek",
            &[("time", &time), ("increment", &inc), ("rated", if rated { "true" } else { "false" }), ("color", color)],
        )?;
        let (tx, done) = mpsc::channel();
        thread::spawn(move || {
            let mut reader = response.into_reader();
            let mut sink = [0u8; 256];
            let result = loop {
                match reader.read(&mut sink) {
                    Ok(0) => break Ok(()),
                    Ok(_) => {}
                    Err(e) => break Err(LichessError::from(e)),
                }
            };
            let _ = tx.send(result);
        });
        Ok(Seek { done })
    }

    /// `GET` a JSON endpoint under the base URL.
    fn get_json<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        let text = self.get(path, "application/json")?.into_string()?;
        Ok(serde_json::from_str(&text)?)
    }

    /// `GET` an ndjson endpoint and stream its records on a background thread.
    /// Blank keep-alive lines are skipped; the receiver ends when the server
    /// closes the stream. (For the Lichess game bridge: event and game streams.)
    pub fn stream<T: DeserializeOwned + Send + 'static>(&self, path: &str) -> Result<Receiver<Result<T>>> {
        let response = self.get(path, "application/x-ndjson")?;
        Ok(ndjson(response.into_reader()))
    }

    fn get(&self, path: &str, accept: &str) -> Result<ureq::Response> {
        let url = format!("{}{}", self.base, path);
        for attempt in 0..2 {
            let mut req = ureq::get(&url)
                .set("User-Agent", "dgtboard-rs")
                .set("Accept", accept);
            if let Some(t) = &self.token {
                req = req.set("Authorization", &format!("Bearer {t}"));
            }
            match req.call() {
                Ok(resp) => return Ok(resp),
                Err(ureq::Error::Status(429, _)) if attempt == 0 => {
                    eprintln!("Lichess rate limit hit (HTTP 429); waiting 60 s before retrying …");
                    thread::sleep(Duration::from_secs(60));
                }
                Err(ureq::Error::Status(429, _)) => return Err(LichessError::RateLimited),
                Err(e) => return Err(e.into()),
            }
        }
        unreachable!("the loop returns on the second attempt")
    }

    /// `POST` a form (possibly empty) under the base URL. HTTP 400 becomes
    /// [`LichessError::Refused`] with Lichess's explanation.
    fn post(&self, path: &str, form: &[(&str, &str)]) -> Result<ureq::Response> {
        let url = format!("{}{}", self.base, path);
        for attempt in 0..2 {
            let mut req = ureq::post(&url)
                .set("User-Agent", "dgtboard-rs")
                .set("Accept", "application/json");
            if let Some(t) = &self.token {
                req = req.set("Authorization", &format!("Bearer {t}"));
            }
            let result = if form.is_empty() { req.call() } else { req.send_form(form) };
            match result {
                Ok(resp) => return Ok(resp),
                Err(ureq::Error::Status(429, _)) if attempt == 0 => {
                    eprintln!("Lichess rate limit hit (HTTP 429); waiting 60 s before retrying …");
                    thread::sleep(Duration::from_secs(60));
                }
                Err(ureq::Error::Status(429, _)) => return Err(LichessError::RateLimited),
                Err(ureq::Error::Status(400, resp)) => {
                    let body = resp.into_string().unwrap_or_default();
                    let reason = serde_json::from_str::<serde_json::Value>(&body)
                        .ok()
                        .and_then(|v| v.get("error").and_then(|e| e.as_str().map(str::to_string)))
                        .unwrap_or(body);
                    return Err(LichessError::Refused(reason.trim().to_string()));
                }
                Err(e) => return Err(e.into()),
            }
        }
        unreachable!("the loop returns on the second attempt")
    }
}

/// Decode an ndjson byte stream on a background thread: one JSON record per
/// non-blank line, delivered through a channel. The thread stops when the
/// reader ends, errors, or the receiver is dropped.
pub fn ndjson<T, R>(reader: R) -> Receiver<Result<T>>
where
    T: DeserializeOwned + Send + 'static,
    R: Read + Send + 'static,
{
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(reader).lines() {
            let item = match line {
                Ok(l) if l.trim().is_empty() => continue, // keep-alive
                Ok(l) => serde_json::from_str::<T>(&l).map_err(LichessError::from),
                Err(e) => Err(LichessError::from(e)),
            };
            if tx.send(item).is_err() {
                break;
            }
        }
    });
    rx
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Write};
    use std::net::TcpListener;

    const DAILY: &str = r#"{"game":{"id":"AHGPPS44","perf":{"key":"blitz","name":"Blitz"},"rated":true,
        "players":[{"name":"GM Alpha","id":"alpha","rating":2650,"color":"white"},
                   {"name":"beta","id":"beta","rating":2600,"color":"black"}],
        "pgn":"e4 c5 Nf3 d6 Bb5+ Bd7 Bxd7+ Qxd7","clock":"5+0"},
        "puzzle":{"id":"PSjmf","rating":1817,"plays":3920,"themes":["crushing","long","middlegame"],
        "fen":"r1bqkb1r/pppp1ppp/2n2n2/4p3/2B1P3/5N2/PPPP1PPP/RNBQK2R w KQkq - 4 4",
        "lastMove":"g8f6","solution":["c4f7","e8f7","f3g5"],"initialPly":6,"extra":1}}"#;

    #[test]
    fn parses_a_puzzle_response_and_ignores_unknown_fields() {
        let p: PuzzleAndGame = serde_json::from_str(DAILY).unwrap();
        assert_eq!(p.puzzle.id, "PSjmf");
        assert_eq!(p.puzzle.last_move, "g8f6");
        assert_eq!(p.puzzle.solution, ["c4f7", "e8f7", "f3g5"]);
        assert_eq!(p.puzzle.initial_ply, 6);
        assert_eq!(p.game.players[0].rating, Some(2650));
        assert_eq!(p.game.perf.name, "Blitz");
        assert_eq!(p.game.clock.as_deref(), Some("5+0"));
    }

    #[test]
    fn minimal_puzzle_json_is_enough() {
        let p: PuzzleAndGame =
            serde_json::from_str(r#"{"game":{},"puzzle":{"id":"x","fen":"8/8/8/8/8/8/8/8 w - -","solution":[]}}"#)
                .unwrap();
        assert_eq!(p.puzzle.rating, 0);
        assert!(p.game.players.is_empty());
    }

    #[test]
    fn ndjson_skips_keepalives_and_reports_bad_lines() {
        #[derive(Deserialize, Debug, PartialEq)]
        struct Rec {
            n: u32,
        }
        let data = "{\"n\":1}\n\n   \n{\"n\":2}\r\nnot json\n{\"n\":3}";
        let rx = ndjson::<Rec, _>(Cursor::new(data.as_bytes().to_vec()));
        let items: Vec<_> = rx.iter().collect();
        assert_eq!(items.len(), 4);
        assert_eq!(items[0].as_ref().unwrap(), &Rec { n: 1 });
        assert_eq!(items[1].as_ref().unwrap(), &Rec { n: 2 });
        assert!(matches!(items[2], Err(LichessError::Json(_))));
        assert_eq!(items[3].as_ref().unwrap(), &Rec { n: 3 });
    }

    /// Serve one canned response on a local port and capture the request head.
    fn one_shot_server(status: &'static str, body: &'static str) -> (String, Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut head = Vec::new();
            let mut buf = [0u8; 1];
            while !head.ends_with(b"\r\n\r\n") && stream.read(&mut buf).unwrap() == 1 {
                head.push(buf[0]);
            }
            let head = String::from_utf8_lossy(&head).to_string();
            // Drain a request body, else closing the socket resets the client.
            let length: usize = head
                .lines()
                .find_map(|l| l.strip_prefix("Content-Length: "))
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(0);
            let mut request_body = vec![0u8; length];
            stream.read_exact(&mut request_body).unwrap();
            tx.send(head).unwrap();
            write!(
                stream,
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
            stream.flush().unwrap();
        });
        (format!("http://{addr}"), rx)
    }

    #[test]
    fn sends_user_agent_and_bearer_token_and_parses() {
        let (base, head) = one_shot_server("200 OK", DAILY);
        let client = Lichess::with_token("lip_secret").with_base_url(base + "/");
        let p = client.puzzle_next(Some("mateIn2"), Some("harder"), Some("white")).unwrap();
        assert_eq!(p.puzzle.id, "PSjmf");
        let head = head.recv().unwrap();
        assert!(head.starts_with("GET /api/puzzle/next?angle=mateIn2&difficulty=harder&color=white HTTP/1.1"), "{head}");
        assert!(head.contains("User-Agent: dgtboard-rs"), "{head}");
        assert!(head.contains("Authorization: Bearer lip_secret"), "{head}");
    }

    #[test]
    fn anonymous_requests_carry_no_authorization() {
        let (base, head) = one_shot_server("200 OK", DAILY);
        let client = Lichess::with_token("   ").with_base_url(base);
        assert!(!client.is_authenticated());
        client.puzzle_by_id("PSjmf").unwrap();
        let head = head.recv().unwrap();
        assert!(head.starts_with("GET /api/puzzle/PSjmf "), "{head}");
        assert!(!head.to_ascii_lowercase().contains("authorization"), "{head}");
    }

    #[test]
    fn http_errors_surface() {
        let (base, _head) = one_shot_server("404 Not Found", "{}");
        let err = Lichess::anonymous().with_base_url(base).puzzle_by_id("nope").unwrap_err();
        assert!(matches!(err, LichessError::Http(_)), "{err}");
    }

    const GAME_FULL: &str = r#"{"type":"gameFull","id":"abcd1234","rated":false,"variant":{"key":"standard"},
        "clock":{"initial":900000,"increment":10000},"speed":"rapid","perf":{"name":"Rapid"},"createdAt":1,
        "white":{"id":"me","name":"Me","rating":1500,"provisional":true},"black":{"aiLevel":1},
        "initialFen":"startpos","state":{"type":"gameState","moves":"e2e4 e7e5","wtime":900000,"btime":890000,
        "winc":10000,"binc":10000,"status":"started"}}"#;

    #[test]
    fn parses_game_stream_records_and_ignores_unknown_ones() {
        match serde_json::from_str::<GameEvent>(GAME_FULL).unwrap() {
            GameEvent::GameFull { id, clock, white, black, initial_fen, state, .. } => {
                assert_eq!(id, "abcd1234");
                assert_eq!(clock.unwrap().label(), "15+10");
                assert_eq!(white.display_name(), "Me");
                assert_eq!(black.display_name(), "Stockfish level 1");
                assert_eq!(initial_fen, "startpos");
                assert_eq!(state.moves, "e2e4 e7e5");
                assert!(state.is_ongoing());
            }
            other => panic!("{other:?}"),
        }
        let state = r#"{"type":"gameState","moves":"e2e4 e7e5 g1f3","wtime":1,"btime":2,"winc":0,"binc":0,
            "status":"resign","winner":"black","bdraw":true,"extra":[]}"#;
        match serde_json::from_str::<GameEvent>(state).unwrap() {
            GameEvent::GameState(s) => {
                assert!(!s.is_ongoing());
                assert_eq!(s.winner.as_deref(), Some("black"));
                assert!(s.bdraw && !s.wdraw);
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            serde_json::from_str::<GameEvent>(r#"{"type":"chatLine","username":"x","text":"gg","room":"player"}"#).unwrap(),
            GameEvent::ChatLine { ref text, .. } if text == "gg"
        ));
        assert!(matches!(
            serde_json::from_str::<GameEvent>(r#"{"type":"somethingNew","foo":1}"#).unwrap(),
            GameEvent::Other
        ));
    }

    #[test]
    fn parses_account_events() {
        let start = r#"{"type":"gameStart","game":{"gameId":"abcd1234","fullId":"abcd1234wxyz","color":"black",
            "fen":"rnbqkbnr/pppppppp/8/8/4P3/8/PPPP1PPP/RNBQKBNR b KQkq - 0 1","isMyTurn":true,"lastMove":"e2e4",
            "speed":"rapid","rated":false,"opponent":{"id":"ai","username":"Stockfish level 1","ai":1},"compat":{"board":true}}}"#;
        match serde_json::from_str::<BoardEvent>(start).unwrap() {
            BoardEvent::GameStart { game } => {
                assert_eq!(game.game_id, "abcd1234");
                assert_eq!(game.my_color(), Color::Black);
                assert_eq!(game.opponent.username, "Stockfish level 1");
                assert_eq!(game.opponent.ai, Some(1));
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            serde_json::from_str::<BoardEvent>(r#"{"type":"challengeCanceled","challenge":{}}"#).unwrap(),
            BoardEvent::Other
        ));
        let playing: Playing = serde_json::from_str(r#"{"nowPlaying":[{"gameId":"x","color":"white"}]}"#).unwrap();
        assert_eq!(playing.now_playing[0].my_color(), Color::White);
    }

    #[test]
    fn posts_moves_and_surfaces_refusals() {
        let (base, head) = one_shot_server("200 OK", r#"{"ok":true}"#);
        let client = Lichess::with_token("lip_secret").with_base_url(base);
        client.make_move("abcd1234", "e2e4").unwrap();
        let head = head.recv().unwrap();
        assert!(head.starts_with("POST /api/board/game/abcd1234/move/e2e4 HTTP/1.1"), "{head}");
        assert!(head.contains("Authorization: Bearer lip_secret"), "{head}");

        let (base, _head) = one_shot_server("400 Bad Request", r#"{"error":"Not your turn, or game already over"}"#);
        let err = Lichess::with_token("t").with_base_url(base).make_move("abcd1234", "e2e4").unwrap_err();
        assert!(matches!(&err, LichessError::Refused(r) if r == "Not your turn, or game already over"), "{err}");
    }

    #[test]
    fn challenge_ai_sends_a_form_and_reads_the_game_id() {
        let (base, head) = one_shot_server("201 Created", r#"{"id":"abcd1234","fullId":"abcd1234wxyz"}"#);
        let client = Lichess::with_token("t").with_base_url(base);
        let game = client.challenge_ai(3, 900, 10, "white").unwrap();
        assert_eq!(game.id, "abcd1234");
        let head = head.recv().unwrap();
        assert!(head.starts_with("POST /api/challenge/ai "), "{head}");
        assert!(head.contains("application/x-www-form-urlencoded"), "{head}");
    }
}
