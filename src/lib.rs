//! `dgtboard` — talk to a DGT electronic chess board over USB / serial.
//!
//! A DGT serial e-board (including the USB models, which expose an FTDI virtual
//! serial port) speaks a simple byte protocol at 9600 baud, 8N1. This crate
//! wraps that protocol:
//!
//! ```no_run
//! use dgtboard::{DgtBoard, Event};
//! use std::time::Duration;
//!
//! let mut board = DgtBoard::open("/dev/cu.usbserial-XXXX")?;
//! // One-shot: read the current position.
//! let pos = board.snapshot(Duration::from_secs(2))?;
//! println!("{}", pos.fen_placement());
//!
//! // Or stream changes live.
//! board.reset()?;
//! board.request_board()?;   // seed the full position
//! board.start_updates()?;   // then receive per-square updates
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

pub mod board;
pub mod game;
pub mod protocol;

pub use board::{Board, Color, Piece, Square};
pub use game::{CastleSide, DetectedMove, MoveKind, MoveTracker};

/// Errors produced by this crate.
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

/// A decoded event from the board.
#[derive(Clone, Debug)]
pub enum Event {
    /// A full board snapshot (reply to [`DgtBoard::request_board`]).
    BoardDump(Board),
    /// A single square changed (sent while in update mode).
    FieldUpdate {
        square: Square,
        /// The piece now on the square, or `None` if it was lifted/emptied.
        piece: Option<Piece>,
    },
    /// Firmware version `(major, minor)`.
    Version(u8, u8),
    /// Board serial number.
    SerialNumber(String),
    /// A clock / time message, payload passed through unparsed.
    Clock(Vec<u8>),
    /// Any other message we don't model: raw id (bit stripped) + payload.
    Other { id: u8, body: Vec<u8> },
}

/// A raw, framed protocol message.
struct RawMessage {
    /// Message id with [`protocol::MESSAGE_BIT`] already stripped.
    id: u8,
    body: Vec<u8>,
}

/// A connection to a DGT board.
pub struct DgtBoard {
    port: Box<dyn SerialPort>,
    buf: Vec<u8>,
    board: Board,
    flip: bool,
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
            buf: Vec::with_capacity(128),
            board: Board::empty(),
            flip: false,
        }
    }

    /// Set whether the board is read rotated 180° (i.e. White sits at the end
    /// *away* from the cable). Applied to both full dumps and field updates so
    /// the cached board and emitted events are always in this orientation.
    pub fn set_flipped(&mut self, flip: bool) {
        self.flip = flip;
    }

    /// Builder form of [`set_flipped`](Self::set_flipped).
    pub fn flipped(mut self, flip: bool) -> Self {
        self.flip = flip;
        self
    }

    /// The last known board state, as maintained from dumps and field updates.
    pub fn board(&self) -> &Board {
        &self.board
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
        match self.take_message()? {
            Some(raw) => Ok(Some(self.decode(raw))),
            None => Ok(None),
        }
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

    /// Pull any bytes waiting on the port into our buffer.
    fn fill(&mut self) -> Result<()> {
        let avail = self.port.bytes_to_read()? as usize;
        if avail == 0 {
            return Ok(());
        }
        let start = self.buf.len();
        self.buf.resize(start + avail, 0);
        match self.port.read(&mut self.buf[start..]) {
            Ok(n) => self.buf.truncate(start + n),
            Err(ref e) if e.kind() == io::ErrorKind::TimedOut => self.buf.truncate(start),
            Err(e) => {
                self.buf.truncate(start);
                return Err(e.into());
            }
        }
        Ok(())
    }

    /// Try to pull one complete framed message out of the buffer.
    fn take_message(&mut self) -> Result<Option<RawMessage>> {
        loop {
            if self.buf.len() < 3 {
                return Ok(None);
            }
            let id = self.buf[0];
            // Resync: a valid message always starts with the message bit set.
            if id & protocol::MESSAGE_BIT == 0 {
                self.buf.remove(0);
                continue;
            }
            // Length is two 7-bit halves and includes the 3-byte header.
            let len = (((self.buf[1] & 0x7f) as usize) << 7) | (self.buf[2] & 0x7f) as usize;
            if len < 3 {
                // Implausible length: drop the header byte and resync.
                self.buf.remove(0);
                continue;
            }
            if self.buf.len() < len {
                return Ok(None);
            }
            let frame: Vec<u8> = self.buf.drain(..len).collect();
            return Ok(Some(RawMessage {
                id: id & !protocol::MESSAGE_BIT,
                body: frame[3..].to_vec(),
            }));
        }
    }

    /// Turn a raw message into an [`Event`], updating the cached board state.
    fn decode(&mut self, raw: RawMessage) -> Event {
        use protocol::msg;
        match raw.id {
            msg::BOARD_DUMP => match Board::from_dump(&raw.body) {
                Ok(mut b) => {
                    if self.flip {
                        b = b.rotated_180();
                    }
                    self.board = b.clone();
                    Event::BoardDump(b)
                }
                Err(_) => Event::Other {
                    id: raw.id,
                    body: raw.body,
                },
            },
            msg::FIELD_UPDATE if raw.body.len() >= 2 && raw.body[0] < 64 => {
                let raw_idx = raw.body[0];
                let square = Square(if self.flip { 63 - raw_idx } else { raw_idx });
                let piece = Piece::from_code(raw.body[1]);
                self.board.set(square, piece);
                Event::FieldUpdate { square, piece }
            }
            msg::VERSION if raw.body.len() >= 2 => Event::Version(raw.body[0], raw.body[1]),
            msg::SERIALNR => {
                Event::SerialNumber(String::from_utf8_lossy(&raw.body).trim().to_string())
            }
            msg::BWTIME => Event::Clock(raw.body),
            _ => Event::Other {
                id: raw.id,
                body: raw.body,
            },
        }
    }
}

/// List the serial ports currently available on the system.
pub fn available_ports() -> Result<Vec<serialport::SerialPortInfo>> {
    Ok(serialport::available_ports()?)
}
