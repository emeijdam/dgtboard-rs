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

The board only knows piece placement, so `fen_placement()` returns just the
first FEN field (no side-to-move / castling / counters).

## CLI

```sh
cargo run -- list                     # show serial ports
cargo run -- snapshot                 # read the position once
cargo run -- snapshot --port /dev/cu.usbserial-XXXX
cargo run -- watch                    # live: print every piece move
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
