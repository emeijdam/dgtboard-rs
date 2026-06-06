//! `dgt` — a small demo CLI on top of the `dgtboard` library.

use std::time::Duration;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use dgtboard::{Board, DgtBoard, Event, IllegalReason, MoveTracker, RefereedGame, Ruling, Status};
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

/// DGT's reported USB vendor id (seen on DGT e-Board / USB boards).
const DGT_VID: u16 = 0x045b;

/// Score a port as a DGT-board candidate; higher is better, `0` = not USB.
fn dgt_score(p: &serialport::SerialPortInfo) -> u32 {
    let SerialPortType::UsbPort(info) = &p.port_type else {
        return 0;
    };
    let mut score = 1; // any USB serial port is a weak candidate
    let looks_dgt = info.vid == DGT_VID
        || info
            .product
            .as_deref()
            .map(|s| s.to_ascii_lowercase().contains("dgt"))
            .unwrap_or(false)
        || info
            .manufacturer
            .as_deref()
            .map(|s| s.to_ascii_lowercase().contains("game technology"))
            .unwrap_or(false);
    if looks_dgt {
        score += 10;
    }
    // On macOS both cu.* and tty.* appear; cu.* is the one you want.
    if p.port_name.contains("cu.") {
        score += 1;
    }
    score
}

/// Pick a port: use the explicit one, else the best-scoring DGT/USB candidate,
/// else fail with guidance.
fn pick_port(explicit: Option<String>) -> Result<String> {
    if let Some(p) = explicit {
        return Ok(p);
    }
    let ports = dgtboard::available_ports().context("listing serial ports")?;
    let mut candidates: Vec<_> = ports.iter().map(|p| (dgt_score(p), p)).collect();
    candidates.retain(|(score, _)| *score > 0);
    candidates.sort_by(|a, b| b.0.cmp(&a.0));

    match candidates.as_slice() {
        [] => bail!("no USB serial ports found. Run `dgt doctor` to diagnose the connection (driver / security), or pass --port."),
        [(score, best), rest @ ..] => {
            // Ambiguous only if a different *device* scores equally well; the
            // cu/tty pair for one device is disambiguated by the cu.* bonus.
            let tie = rest.iter().any(|(s, _)| s == score);
            if tie {
                let names: Vec<_> = candidates.iter().map(|(_, p)| p.port_name.as_str()).collect();
                bail!(
                    "multiple equally-likely USB ports found ({}); pass --port to choose one",
                    names.join(", ")
                );
            }
            eprintln!("Auto-selected port: {}", best.port_name);
            Ok(best.port_name.clone())
        }
    }
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

fn announce(status: Status) {
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

fn reason_text(reason: IllegalReason) -> &'static str {
    match reason {
        IllegalReason::NotYourTurn => "it's not your turn",
        IllegalReason::NoPieceThere => "there was no piece on that square",
        IllegalReason::MustGetOutOfCheck => "your king is in check",
        IllegalReason::OwnPieceOnTarget => "your own piece is already there",
        IllegalReason::IllegalMove => "that piece can't move like that",
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
                None => {}
            },
            Some(_) => {}
            None => std::thread::sleep(Duration::from_millis(20)),
        }
    }
}
