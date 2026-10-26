//! A small Lichess API client: puzzles now, ndjson streams for the game bridge.
//!
//! Only what the `dgt` CLI needs is modelled. Requests are plain blocking HTTP
//! ([`ureq`]); a personal API token is optional — without one the puzzle
//! endpoints still work, with one Lichess serves puzzles the account hasn't
//! seen. The token is never logged or printed.
//!
//! Lichess asks for one request at a time and answers `429 Too Many Requests`
//! when that's exceeded; the client then waits a minute and retries once.

use std::io::{self, BufRead, BufReader, Read};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde::Deserialize;

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
            tx.send(String::from_utf8_lossy(&head).to_string()).unwrap();
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
}
