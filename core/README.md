# dgtboard-core

The DGT electronic chess board protocol as **pure logic, with no I/O**: byte
framing, board/piece decoding, FEN, and move detection. No dependencies, no
transport — so it runs natively, on a server, or in the browser via
WebAssembly.

```rust
use dgtboard_core::{Decoder, Event, MoveTracker};

let mut decoder = Decoder::new();
decoder.push(&bytes_from_the_board);
while let Some(event) = decoder.poll() {
    if let Event::BoardDump(board) = &event {
        println!("{}", board.fen_placement());
    }
}
```

- For the native USB / serial transport and the `dgt` CLI, use the
  [`dgtboard`](https://crates.io/crates/dgtboard) crate.
- For browser use, see `dgtboard-wasm` and the Web Serial demo in the repo.
