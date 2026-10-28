//! `dgt` — a small demo CLI on top of the `dgtboard` library.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use dgtboard::lichess::Lichess;
use dgtboard::lichess_game::{self, reason_text, Start, TimeControl};
use dgtboard::puzzle::{announce, BoardInput, PuzzleSource};
use dgtboard::{pick_port, Board, DgtBoard, Event, MoveTracker, RefereedGame, Ruling};
use serialport::SerialPortType;

mod doctor;

#[derive(Parser)]
#[command(name = "dgt", about = "Recognise and read a DGT chess board over USB / serial")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Diagnose connection problems: USB device, serial port, driver, security.
    Doctor {
        /// Open System Settings at the driver-approval screen.
        #[arg(long)]
        open_settings: bool,
    },
    /// List available serial ports (and flag likely DGT/USB boards).
    List,
    /// Connect, read the position once, print board + FEN, exit.
    Snapshot {
        /// Serial port path. Auto-detected if omitted.
        #[arg(short, long)]
        port: Option<String>,
        /// Read the board rotated 180° (White at the end away from the cable).
        #[arg(short, long)]
        flip: bool,
    },
    /// Connect and continuously print raw per-square changes as pieces move.
    Watch {
        /// Serial port path. Auto-detected if omitted.
        #[arg(short, long)]
        port: Option<String>,
        /// Read the board rotated 180° (White at the end away from the cable).
        #[arg(short, long)]
        flip: bool,
    },
    /// Connect and print detected chess moves (UCI + description) as they're played.
    Moves {
        /// Serial port path. Auto-detected if omitted.
        #[arg(short, long)]
        port: Option<String>,
        /// Read the board rotated 180° (White at the end away from the cable).
        #[arg(short, long)]
        flip: bool,
    },
    /// Referee a game from the start position: validate moves, flag illegal ones,
    /// and announce check / checkmate / stalemate.
    Referee {
        /// Serial port path. Auto-detected if omitted.
        #[arg(short, long)]
        port: Option<String>,
        /// Read the board rotated 180° (White at the end away from the cable).
        #[arg(short, long)]
        flip: bool,
    },
    /// Solve Lichess puzzles on the board. The opponent's replies are printed
    /// here for you to play on the board. Trainer only: nothing is sent back
    /// to Lichess, so your puzzle rating is unaffected.
    Puzzle {
        /// Serial port path. Auto-detected if omitted.
        #[arg(short, long)]
        port: Option<String>,
        /// Read the board rotated 180° (White at the end away from the cable).
        #[arg(short, long)]
        flip: bool,
        /// Today's daily puzzle (then random ones).
        #[arg(long, conflicts_with_all = ["id", "theme", "difficulty", "color"])]
        daily: bool,
        /// One puzzle by id (then random ones).
        #[arg(long, conflicts_with_all = ["theme", "difficulty", "color"])]
        id: Option<String>,
        /// Puzzle theme or opening for random puzzles, e.g. mateIn2, fork, endgame.
        #[arg(long)]
        theme: Option<String>,
        /// Difficulty relative to your rating.
        #[arg(long, value_parser = ["easiest", "easier", "normal", "harder", "hardest"])]
        difficulty: Option<String>,
        /// Which colour you solve for.
        #[arg(long, value_parser = ["white", "black"])]
        color: Option<String>,
        /// No board: type moves in UCI (e2e4, e1g1, e7e8q) instead.
        #[arg(long)]
        keyboard: bool,
        /// File holding a Lichess API token (else $LICHESS_TOKEN, else anonymous).
        /// With a token Lichess serves puzzles you haven't seen.
        #[arg(long)]
        token_file: Option<PathBuf>,
    },
    /// Play Lichess games on the board (Board API, token scope board:play).
    /// Your moves are sent as you play them; the opponent's are printed here
    /// for you to execute. Quitting leaves the game running on Lichess.
    Lichess {
        /// Serial port path. Auto-detected if omitted.
        #[arg(short, long)]
        port: Option<String>,
        /// Read the board rotated 180° (White at the end away from the cable).
        #[arg(short, long)]
        flip: bool,
        /// Challenge Stockfish at this level instead of waiting for a game.
        #[arg(long, value_name = "LEVEL", value_parser = clap::value_parser!(u8).range(1..=8), conflicts_with = "seek")]
        ai: Option<u8>,
        /// Clock for --ai, as MIN+INC.
        #[arg(long, default_value = "15+10", value_name = "MIN+INC")]
        time: String,
        /// Seek a human opponent with this clock (rapid or slower, e.g. 15+10).
        #[arg(long, value_name = "MIN+INC")]
        seek: Option<String>,
        /// Your colour for --ai / --seek.
        #[arg(long, default_value = "random", value_parser = ["white", "black", "random"])]
        color: String,
        /// Seek a rated game (with --seek; casual otherwise).
        #[arg(long, requires = "seek")]
        rated: bool,
        /// No board: type moves in UCI (e2e4, e1g1, e7e8q) instead.
        #[arg(long)]
        keyboard: bool,
        /// File holding the Lichess API token (else $LICHESS_TOKEN).
        #[arg(long)]
        token_file: Option<PathBuf>,
    },
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Doctor { open_settings } => {
            doctor::run_doctor(open_settings);
            Ok(())
        }
        Command::List => list_ports(),
        Command::Snapshot { port, flip } => snapshot(port, flip),
        Command::Watch { port, flip } => watch(port, flip),
        Command::Moves { port, flip } => moves(port, flip),
        Command::Referee { port, flip } => referee(port, flip),
        Command::Puzzle {
            port,
            flip,
            daily,
            id,
            theme,
            difficulty,
            color,
            keyboard,
            token_file,
        } => {
            let lichess = match token_file {
                Some(path) => Lichess::with_token_file(&path)
                    .with_context(|| format!("reading token file {}", path.display()))?,
                None => Lichess::from_env(),
            };
            let source = if daily {
                PuzzleSource::Daily
            } else if let Some(id) = id {
                PuzzleSource::Id(id)
            } else {
                PuzzleSource::Next {
                    angle: theme,
                    difficulty,
                    color,
                }
            };
            let mut input = if keyboard {
                BoardInput::keyboard()
            } else {
                BoardInput::open_serial(port, flip)?
            };
            dgtboard::puzzle::run(&mut input, &lichess, source)
        }
        Command::Lichess {
            port,
            flip,
            ai,
            time,
            seek,
            color,
            rated,
            keyboard,
            token_file,
        } => {
            let lichess = match token_file {
                Some(path) => Lichess::with_token_file(&path)
                    .with_context(|| format!("reading token file {}", path.display()))?,
                None => Lichess::from_env(),
            };
            let parse = |s: &str| TimeControl::parse(s).map_err(|e| anyhow::anyhow!("--time/--seek {e}"));
            let start = if let Some(level) = ai {
                Start::Ai {
                    level,
                    time: parse(&time)?,
                }
            } else if let Some(tc) = seek {
                Start::Seek {
                    time: parse(&tc)?,
                    rated,
                }
            } else {
                Start::Wait
            };
            let mut input = if keyboard {
                BoardInput::keyboard()
            } else {
                BoardInput::open_serial(port, flip)?
            };
            lichess_game::run(&mut input, &lichess, lichess_game::Options { start, color })
        }
    }
}

fn list_ports() -> Result<()> {
    let ports = dgtboard::available_ports().context("listing serial ports")?;
    if ports.is_empty() {
        println!("No serial ports found.");
        return Ok(());
    }
    println!("Available serial ports:");
    for p in &ports {
        match &p.port_type {
            SerialPortType::UsbPort(info) => {
                let product = info.product.as_deref().unwrap_or("?");
                let manufacturer = info.manufacturer.as_deref().unwrap_or("?");
                println!(
                    "  {}  [USB {:04x}:{:04x} {} / {}]",
                    p.port_name, info.vid, info.pid, manufacturer, product
                );
            }
            other => println!("  {}  [{:?}]", p.port_name, other),
        }
    }
    Ok(())
}

fn print_board(board: &Board) {
    print!("{board}");
    println!("FEN: {}", board.fen_placement());
}

fn snapshot(port: Option<String>, flip: bool) -> Result<()> {
    let path = pick_port(port)?;
    let mut board = DgtBoard::open(&path)
        .with_context(|| format!("opening {path}"))?
        .flipped(flip);
    let position = board
        .snapshot(Duration::from_secs(3))
        .context("reading board (is the board powered and connected?)")?;
    print_board(&position);
    Ok(())
}

fn watch(port: Option<String>, flip: bool) -> Result<()> {
    let path = pick_port(port)?;
    let mut board = DgtBoard::open(&path)
        .with_context(|| format!("opening {path}"))?
        .flipped(flip);

    board.reset()?;
    board.request_version()?;
    board.request_board()?; // seed the full position
    board.start_updates()?; // then stream per-square changes

    println!("Watching {path}. Move pieces on the board; Ctrl-C to stop.\n");

    loop {
        match board.poll()? {
            Some(Event::BoardDump(b)) => {
                println!("== position ==");
                print_board(&b);
                println!();
            }
            Some(Event::FieldUpdate { square, piece }) => {
                match piece {
                    Some(p) => println!("{square}: {} ({})", p.glyph(), p.fen_char()),
                    None => println!("{square}: cleared"),
                }
                println!("FEN: {}\n", board.board().fen_placement());
            }
            Some(Event::Version(major, minor)) => {
                println!("Board firmware v{major}.{minor}\n");
            }
            Some(Event::SerialNumber(sn)) => println!("Serial: {sn}\n"),
            Some(Event::Clock(_)) => { /* ignore clock traffic */ }
            Some(Event::Other { .. }) => { /* ignore unmodelled messages */ }
            None => std::thread::sleep(Duration::from_millis(20)),
        }
    }
}

fn moves(port: Option<String>, flip: bool) -> Result<()> {
    let path = pick_port(port)?;
    let mut board = DgtBoard::open(&path)
        .with_context(|| format!("opening {path}"))?
        .flipped(flip);

    // Seed the tracker from the current position.
    let start = board
        .snapshot(Duration::from_secs(3))
        .context("reading initial board (is the board powered and connected?)")?;
    println!("Starting position:");
    print_board(&start);
    let mut tracker = MoveTracker::new(start);
    match tracker.side_to_move() {
        Some(side) => println!("{side} to move.\n"),
        None => println!("(Not the standard start \u{2014} side to move inferred from first move.)\n"),
    }

    board.start_updates()?;
    println!("Play moves on the board; Ctrl-C to stop.\n");

    let mut ply = 0u32;
    loop {
        match board.poll()? {
            Some(Event::FieldUpdate { .. }) => {
                if let Some(mv) = tracker.update(board.board()) {
                    ply += 1;
                    println!(
                        "{ply:>3}. {color:<5} {uci:<6} {desc}",
                        color = mv.color,
                        uci = mv.uci(),
                        desc = mv.describe()
                    );
                    println!("     FEN: {}\n", tracker.confirmed().fen_placement());
                }
            }
            // Tolerate a board re-dump (e.g. after replug) by re-seeding.
            Some(Event::BoardDump(b)) => {
                tracker = MoveTracker::new(b);
            }
            Some(_) => {}
            None => std::thread::sleep(Duration::from_millis(20)),
        }
    }
}

fn referee(port: Option<String>, flip: bool) -> Result<()> {
    let path = pick_port(port)?;
    let mut board = DgtBoard::open(&path)
        .with_context(|| format!("opening {path}"))?
        .flipped(flip);

    let start = board
        .snapshot(Duration::from_secs(3))
        .context("reading initial board (is the board powered and connected?)")?;
    println!("Starting position:");
    print_board(&start);
    if start != Board::startpos() {
        println!(
            "⚠ Board isn't in the standard starting position — referee mode expects a fresh game."
        );
        println!("  Set the pieces up and restart, or use `dgt moves` for free play.\n");
    }

    let mut game = RefereedGame::new();
    board.start_updates()?;
    println!("Refereeing — play a game. Illegal moves are flagged; Ctrl-C to stop.\n");

    let mut ply = 0u32;
    loop {
        match board.poll()? {
            Some(Event::FieldUpdate { .. }) => match game.update(board.board()) {
                Some(Ruling::Legal { san, status, .. }) => {
                    ply += 1;
                    let mover = if ply % 2 == 1 { "White" } else { "Black" };
                    println!("{ply:>3}. {mover:<5} {san}");
                    announce(status);
                }
                Some(Ruling::Illegal { uci, reason, .. }) => {
                    println!(
                        "  ⚠ ILLEGAL: {uci} — {}. Put the piece back to continue.",
                        reason_text(reason)
                    );
                }
                Some(Ruling::BackInSync) => println!("  ✓ Back in sync.\n"),
                Some(_) | None => {}
            },
            Some(_) => {}
            None => std::thread::sleep(Duration::from_millis(20)),
        }
    }
}
