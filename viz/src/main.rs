//! Native launcher for the 3D viewer.
//!
//! `cargo run -p dgtboard-bevy --bin dgt3d` opens a window seeded with the start
//! position and, if a DGT board is plugged in, follows it live. Orbit with
//! left-drag, zoom with the scroll wheel; `Tab` swaps in the flat 2D board,
//! `F` views from Black's side. With `--puzzle` or `--lichess` / `--ai` /
//! `--seek` the window runs the Lichess session the `dgt` CLI would, drawing
//! the opponent's move, the squares to set up, and a HUD over the board.

#[cfg(not(target_arch = "wasm32"))]
use std::path::PathBuf;

#[cfg(not(target_arch = "wasm32"))]
use clap::Parser;

#[cfg(not(target_arch = "wasm32"))]
#[derive(Parser)]
#[command(name = "dgt3d", about = "A live DGT chess board as a real-time 3D scene")]
struct Args {
    /// Serial port path. Auto-detected if omitted.
    #[arg(short, long)]
    port: Option<String>,
    /// Read the board rotated 180° (White at the end away from the cable).
    #[arg(short, long)]
    flip: bool,
    /// No board: type moves in UCI (e2e4, e1g1, e7e8q) in the terminal instead.
    #[arg(long)]
    keyboard: bool,
    /// File holding the Lichess API token (else $LICHESS_TOKEN, else anonymous).
    #[arg(long, value_name = "PATH")]
    token_file: Option<PathBuf>,

    /// Play Lichess games: wait for one started on lichess.org.
    #[arg(long, conflicts_with_all = ["ai", "seek", "puzzle"])]
    lichess: bool,
    /// Play a Lichess game against Stockfish at this level (1–8).
    #[arg(long, value_name = "LEVEL", conflicts_with_all = ["seek", "puzzle"])]
    ai: Option<u8>,
    /// Clock for --ai, as MIN+INC.
    #[arg(long, default_value = "15+10", value_name = "MIN+INC")]
    time: String,
    /// Seek a human opponent on Lichess with this clock (rapid or slower).
    #[arg(long, value_name = "MIN+INC", conflicts_with = "puzzle")]
    seek: Option<String>,
    /// Seek a rated game (with --seek; casual otherwise).
    #[arg(long)]
    rated: bool,
    /// Your colour: white, black or random (games), white or black (puzzles).
    #[arg(long)]
    color: Option<String>,

    /// Solve Lichess puzzles.
    #[arg(long)]
    puzzle: bool,
    /// Today's daily puzzle first (with --puzzle).
    #[arg(long, requires = "puzzle")]
    daily: bool,
    /// One puzzle by id first (with --puzzle).
    #[arg(long, value_name = "ID", requires = "puzzle")]
    id: Option<String>,
    /// Puzzle theme or opening for random puzzles, e.g. mateIn2, fork, endgame.
    #[arg(long, requires = "puzzle")]
    theme: Option<String>,
    /// Puzzle difficulty relative to your rating: easiest, easier, normal, harder, hardest.
    #[arg(long, requires = "puzzle")]
    difficulty: Option<String>,
}

#[cfg(not(target_arch = "wasm32"))]
fn main() {
    use dgtboard::lichess_game::{Start, TimeControl};
    use dgtboard::puzzle::PuzzleSource;
    use dgtboard_bevy::session::{Connection, Mode};

    let args = Args::parse();
    let time = |s: &str| match TimeControl::parse(s) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("dgt3d: {e}");
            std::process::exit(2);
        }
    };
    let color = args.color.clone().unwrap_or_else(|| "random".into());
    let mode = if args.puzzle {
        Some(Mode::Puzzle(if args.daily {
            PuzzleSource::Daily
        } else if let Some(id) = args.id.clone() {
            PuzzleSource::Id(id)
        } else {
            PuzzleSource::Next {
                angle: args.theme.clone(),
                difficulty: args.difficulty.clone(),
                color: args.color.clone(),
            }
        }))
    } else if let Some(level) = args.ai {
        Some(Mode::Lichess {
            start: Start::Ai {
                level,
                time: time(&args.time),
            },
            color,
        })
    } else if let Some(seek) = &args.seek {
        Some(Mode::Lichess {
            start: Start::Seek {
                time: time(seek),
                rated: args.rated,
            },
            color,
        })
    } else if args.lichess {
        Some(Mode::Lichess {
            start: Start::Wait,
            color,
        })
    } else {
        None
    };
    dgtboard_bevy::run(dgtboard_bevy::Options {
        connection: Connection {
            port: args.port,
            flip: args.flip,
            keyboard: args.keyboard,
            token_file: args.token_file,
        },
        mode,
    });
}

/// The browser build is the cdylib (see `wasm_api`); this bin has no role there.
#[cfg(target_arch = "wasm32")]
fn main() {}
