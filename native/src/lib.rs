//! `dgtboard` — talk to a DGT electronic chess board over USB / serial.
//!
//! A DGT serial e-board (including the USB models, which expose an FTDI virtual
//! serial port) speaks a simple byte protocol at 9600 baud, 8N1. This crate is
//! the native serial transport on top of the I/O-free [`dgtboard_core`]: it
//! reads bytes from a port, feeds them to a [`Decoder`], and gives you
//! [`Event`]s and board state. All the protocol/board/move-detection types are
//! re-exported from the core, so you can `use dgtboard::{...}` for everything.
//!
//! ```no_run
//! use dgtboard::{DgtBoard, Event};
//! use std::time::Duration;
//!
//! let mut board = DgtBoard::open("/dev/cu.usbserial-XXXX")?;
//! let pos = board.snapshot(Duration::from_secs(2))?;
//! println!("{}", pos.fen_placement());
//!
//! board.start_updates()?;
//! loop {
//!     if let Some(Event::FieldUpdate { square, piece }) = board.poll()? {
//!         println!("{square}: {piece:?}");
//!     }
//! }
//! # Ok::<(), dgtboard::Error>(())
//! ```

use std::io;
use std::time::{Duration, Instant};

use serialport::SerialPort;

// Re-export the whole core API so downstream code only needs `dgtboard`.
pub use dgtboard_core::{
    board, decoder, game, protocol, rules, Board, CastleSide, Color, Decoder, DetectedMove, Event,
    MoveKind, MoveTracker, Piece, RefereedGame, Ruling, Square, Status,
};

/// Errors produced by the native transport.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("serial port error: {0}")]
    Serial(#[from] serialport::Error),
    #[error("io error: {0}")]
    Io(#[from] io::Error),
    #[error("timed out waiting for {0}")]
    Timeout(&'static str),
    #[error("malformed message: {0}")]
    Protocol(String),
}

/// Convenience alias.
pub type Result<T> = std::result::Result<T, Error>;

/// A connection to a DGT board over a serial port.
pub struct DgtBoard {
    port: Box<dyn SerialPort>,
    decoder: Decoder,
}

impl DgtBoard {
    /// Open a board on the given serial port path (e.g. `/dev/cu.usbserial-XXXX`
    /// on macOS, `/dev/ttyUSB0` on Linux, `COM3` on Windows).
    pub fn open(path: &str) -> Result<DgtBoard> {
        let port = serialport::new(path, 9600)
            .data_bits(serialport::DataBits::Eight)
            .parity(serialport::Parity::None)
            .stop_bits(serialport::StopBits::One)
            .flow_control(serialport::FlowControl::None)
            .timeout(Duration::from_millis(100))
            .open()?;
        Ok(DgtBoard::from_port(port))
    }

    /// Wrap an already-configured serial port.
    pub fn from_port(port: Box<dyn SerialPort>) -> DgtBoard {
        DgtBoard {
            port,
            decoder: Decoder::new(),
        }
    }

    /// Set whether the board is read rotated 180° (i.e. White sits at the end
    /// *away* from the cable).
    pub fn set_flipped(&mut self, flip: bool) {
        self.decoder.set_flipped(flip);
    }

    /// Builder form of [`set_flipped`](Self::set_flipped).
    pub fn flipped(mut self, flip: bool) -> Self {
        self.decoder.set_flipped(flip);
        self
    }

    /// The last known board state, as maintained from dumps and field updates.
    pub fn board(&self) -> &Board {
        self.decoder.board()
    }

    fn send(&mut self, command: u8) -> Result<()> {
        self.port.write_all(&[command])?;
        self.port.flush()?;
        Ok(())
    }

    /// Put the board into idle mode (it then stays silent until queried).
    pub fn reset(&mut self) -> Result<()> {
        self.send(protocol::cmd::SEND_RESET)
    }

    /// Request a one-shot full board dump.
    pub fn request_board(&mut self) -> Result<()> {
        self.send(protocol::cmd::SEND_BRD)
    }

    /// Enter update mode: the board streams a [`Event::FieldUpdate`] for every
    /// square change (plus clock messages).
    pub fn start_updates(&mut self) -> Result<()> {
        self.send(protocol::cmd::SEND_UPDATE_BRD)
    }

    /// Ask for the firmware version (arrives later as [`Event::Version`]).
    pub fn request_version(&mut self) -> Result<()> {
        self.send(protocol::cmd::SEND_VERSION)
    }

    /// Ask for the serial number (arrives later as [`Event::SerialNumber`]).
    pub fn request_serial(&mut self) -> Result<()> {
        self.send(protocol::cmd::RETURN_SERIALNR)
    }

    /// Non-blocking: read whatever bytes are available and return the next
    /// decoded event, or `None` if a full message isn't buffered yet.
    pub fn poll(&mut self) -> Result<Option<Event>> {
        self.fill()?;
        Ok(self.decoder.poll())
    }

    /// Blocking convenience: reset, request a board dump, and return it.
    pub fn snapshot(&mut self, timeout: Duration) -> Result<Board> {
        self.reset()?;
        self.request_board()?;
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            match self.poll()? {
                Some(Event::BoardDump(b)) => return Ok(b),
                Some(_) => {}
                None => std::thread::sleep(Duration::from_millis(10)),
            }
        }
        Err(Error::Timeout("board dump"))
    }

    /// Pull any bytes waiting on the port into the decoder.
    fn fill(&mut self) -> Result<()> {
        let avail = self.port.bytes_to_read()? as usize;
        if avail == 0 {
            return Ok(());
        }
        let mut tmp = vec![0u8; avail];
        match self.port.read(&mut tmp) {
            Ok(n) => self.decoder.push(&tmp[..n]),
            Err(ref e) if e.kind() == io::ErrorKind::TimedOut => {}
            Err(e) => return Err(e.into()),
        }
        Ok(())
    }
}

/// List the serial ports currently available on the system.
pub fn available_ports() -> Result<Vec<serialport::SerialPortInfo>> {
    Ok(serialport::available_ports()?)
}
