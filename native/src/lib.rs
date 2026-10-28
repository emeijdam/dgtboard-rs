//! `dgtboard` — talk to a DGT electronic chess board over USB / serial.
//!
//! A DGT serial e-board (including the USB models, which expose an FTDI virtual
//! serial port) speaks a simple byte protocol at 9600 baud, 8N1. This crate is
//! the native serial transport on top of the I/O-free [`dgtboard_core`]: it
//! reads bytes from a port, feeds them to a [`Decoder`], and gives you
//! [`Event`]s and board state. All the protocol/board/move-detection types are
//! re-exported from the core, so you can `use dgtboard::{...}` for everything.
//!
//! Beyond the transport: [`pick_port`] auto-selects the board's USB port,
//! [`lichess`] is a small Lichess API client (puzzles; the Board API),
//! [`puzzle`] solves Lichess puzzles on the board (or from the keyboard), and
//! [`lichess_game`] plays Lichess games from it.
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

use serialport::{SerialPort, SerialPortType};

pub mod lichess;
pub mod lichess_game;
pub mod puzzle;
pub mod ui;

// Re-export the whole core API so downstream code only needs `dgtboard`.
pub use dgtboard_core::{
    board, decoder, game, protocol, remote, rules, Board, CastleSide, Color, Decoder, DetectedMove,
    Event, IllegalReason, MoveKind, MoveTracker, Piece, RefereedGame, RemoteEvent, RemoteGame,
    Ruling, Square, Status,
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
    #[error("{0}")]
    Port(String),
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

/// DGT's reported USB vendor id (seen on DGT e-Board / USB boards).
pub const DGT_VID: u16 = 0x045b;

/// Score a port as a DGT-board candidate; higher is better, `0` = not USB.
pub fn dgt_score(p: &serialport::SerialPortInfo) -> u32 {
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

/// Pick a port: use the explicit one, else the best-scoring DGT/USB candidate
/// (announced on stderr), else fail with guidance.
pub fn pick_port(explicit: Option<String>) -> Result<String> {
    if let Some(p) = explicit {
        return Ok(p);
    }
    let ports = available_ports()?;
    let mut candidates: Vec<_> = ports.iter().map(|p| (dgt_score(p), p)).collect();
    candidates.retain(|(score, _)| *score > 0);
    candidates.sort_by(|a, b| b.0.cmp(&a.0));

    match candidates.as_slice() {
        [] => Err(Error::Port(
            "no USB serial ports found. Run `dgt doctor` to diagnose the connection (driver / security), or pass --port.".into(),
        )),
        [(score, best), rest @ ..] => {
            // Ambiguous only if a different *device* scores equally well; the
            // cu/tty pair for one device is disambiguated by the cu.* bonus.
            let tie = rest.iter().any(|(s, _)| s == score);
            if tie {
                let names: Vec<_> = candidates.iter().map(|(_, p)| p.port_name.as_str()).collect();
                return Err(Error::Port(format!(
                    "multiple equally-likely USB ports found ({}); pass --port to choose one",
                    names.join(", ")
                )));
            }
            eprintln!("Auto-selected port: {}", best.port_name);
            Ok(best.port_name.clone())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serialport::{SerialPortInfo, UsbPortInfo};

    fn usb(name: &str, vid: u16, product: Option<&str>) -> SerialPortInfo {
        SerialPortInfo {
            port_name: name.into(),
            port_type: SerialPortType::UsbPort(UsbPortInfo {
                vid,
                pid: 0x6001,
                serial_number: None,
                manufacturer: None,
                product: product.map(str::to_string),
            }),
        }
    }

    #[test]
    fn dgt_vendor_outscores_generic_usb_and_cu_beats_tty() {
        let generic = usb("/dev/cu.usbserial-1", 0x0403, None);
        let dgt_tty = usb("/dev/tty.usbserial-2", DGT_VID, None);
        let dgt_cu = usb("/dev/cu.usbserial-2", DGT_VID, None);
        let by_name = usb("/dev/ttyUSB0", 0x0403, Some("DGT e-Board"));
        let not_usb = SerialPortInfo {
            port_name: "/dev/cu.Bluetooth".into(),
            port_type: SerialPortType::Unknown,
        };
        assert_eq!(dgt_score(&not_usb), 0);
        assert!(dgt_score(&generic) < dgt_score(&dgt_tty));
        assert!(dgt_score(&dgt_tty) < dgt_score(&dgt_cu));
        assert_eq!(dgt_score(&by_name), dgt_score(&dgt_tty));
    }

    #[test]
    fn explicit_port_wins() {
        assert_eq!(pick_port(Some("COM3".into())).unwrap(), "COM3");
    }
}
