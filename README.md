# dgtboard

Read a [DGT](https://www.digitalgametechnology.com/) electronic chess board over
USB / serial, in Rust.

DGT serial e-boards — including the USB models, which expose an FTDI virtual
serial port — speak a small byte protocol at **9600 baud, 8N1**. This crate
implements that protocol and ships a demo CLI (`dgt`).

## Library

```rust
use dgtboard::{DgtBoard, Event};
use std::time::Duration;

let mut board = DgtBoard::open("/dev/cu.usbserial-XXXX")?;

// One-shot snapshot:
let pos = board.snapshot(Duration::from_secs(2))?;
println!("{}", pos);                 // ASCII board
println!("{}", pos.fen_placement()); // e.g. rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR

// Or stream live changes:
board.reset()?;
board.request_board()?; // seed full position
board.start_updates()?; // then per-square updates
loop {
    if let Some(Event::FieldUpdate { square, piece }) = board.poll()? {
        println!("{square}: {piece:?}");
    }
}
```

### Move detection

`MoveTracker` reconstructs whole chess moves from the per-square update stream,
emitting one `DetectedMove` once the board settles into a position exactly one
move ahead:

```rust
use dgtboard::{DgtBoard, Event, MoveTracker};
use std::time::Duration;

let mut board = DgtBoard::open("/dev/cu.usbserial-XXXX")?;
let start = board.snapshot(Duration::from_secs(2))?;
let mut tracker = MoveTracker::new(start); // White-to-move if it's the start position
board.start_updates()?;
loop {
    if let Some(Event::FieldUpdate { .. }) = board.poll()? {
        if let Some(mv) = tracker.update(board.board()) {
            println!("{}  {}", mv.uci(), mv.describe()); // e.g. "e2e4  ♙ e2-e4"
        }
    }
}
```

It classifies quiet moves, captures, en passant, castling, and promotion (the
hardware identifies the promoted piece). Intermediate states — a piece lifted,
a half-finished castle — are deferred until the move completes, and putting a
piece back without moving it produces nothing.

This is a geometric reconstruction, not a chess engine: it assumes the moves
played are legal and works out *which* move happened. It doesn't validate
legality, detect check/mate, or emit SAN. Two known edge cases: it can't know
whose turn it is if you start mid-game (it infers side-to-move from the first
move's colour), and castling is recognised most reliably when the **king moves
first** (the usual touch-move order).

The board only knows piece placement, so `fen_placement()` returns just the
first FEN field (no side-to-move / castling / counters).

## First time connecting? Run the doctor

DGT boards use an FTDI USB-serial chip with DGT's **custom** USB vendor id, so
macOS's built-in FTDI support doesn't bind to it. You have to install the FTDI
VCP driver and then *approve* it in System Settings — two easily-missed steps.
`dgt doctor` checks the whole chain and tells you exactly where you're stuck:

```sh
cargo run -- doctor
cargo run -- doctor --open-settings   # also jump to the driver-approval screen
```

```
[1/4] USB device   ✓  Board detected: DGT e-Board (VID 045b PID 8111)
[2/4] Serial port  ✓  /dev/cu.usbmodem01
[3/4] Driver       ✓  com.ftdi.vcp.dext [activated enabled]
[4/4] Security     ✓  Driver approved.
--------------------------------------------------
✓ Ready. The board is connected. Try:  dgt snapshot
```

If a step fails it points you at the fix: a data-capable cable, the FTDI VCP
driver download (<https://ftdichip.com/drivers/vcp-drivers/>), or the macOS
driver-approval screen (System Settings → General → Login Items & Extensions →
Driver Extensions on macOS 15+, or Privacy & Security on macOS 13–14). After
approving a driver, unplug and replug the board.

## CLI

```sh
cargo run -- doctor                   # diagnose connection / driver / security
cargo run -- list                     # show serial ports
cargo run -- snapshot                 # read the position once
cargo run -- snapshot --port /dev/cu.usbserial-XXXX
cargo run -- watch                    # live: print raw per-square changes
cargo run -- moves                    # live: print detected chess moves (UCI + description)
```

`snapshot` and `watch` auto-select the port when exactly one USB serial device
is present; otherwise pass `--port` (use `list` to find it).

## Protocol notes

- Framing: byte 0 = message id with `0x80` set; bytes 1–2 are 7-bit length
  halves, `len = (b1 << 7) | b2`, length includes the 3-byte header.
- Square numbering: `0` = a8 .. `63` = h1 (row-major, rank 8 first) — already
  FEN order.
- Commands used: `SEND_RESET 0x40`, `SEND_BRD 0x42`, `SEND_UPDATE_BRD 0x44`,
  `SEND_VERSION 0x4d`. Messages handled: `BOARD_DUMP 0x06`,
  `FIELD_UPDATE 0x0e`, `VERSION 0x13`, `SERIALNR 0x11`, `BWTIME 0x0d`.

## License

MIT OR Apache-2.0
