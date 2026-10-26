//! Native transport: read a DGT board over its serial port and feed the raw
//! bytes into the shared [`QUEUE`](super::QUEUE), exactly as the browser's
//! Web Serial reader does for the wasm build.
//!
//! The port is auto-detected with [`dgtboard::pick_port`] (the same rules as
//! the `dgt` CLI). With no board the scene simply keeps its start-position
//! preview, and a hint points at `dgt doctor`.

use std::io::{self, Read, Write};
use std::thread;
use std::time::Duration;

use dgtboard_core::protocol::cmd;
use serialport::SerialPort;

use super::QUEUE;

/// Start the reader thread. Never blocks the caller; failures are reported
/// on stderr and leave the preview in place.
pub fn spawn_reader(port: Option<String>) {
    thread::Builder::new()
        .name("dgt-serial".into())
        .spawn(move || {
            if let Err(e) = read_forever(port) {
                eprintln!("dgt3d: {e}");
                eprintln!("dgt3d: showing the start position; plug in a board and restart, or run `dgt doctor`.");
            }
        })
        .expect("spawn serial reader thread");
}

fn open(path: &str) -> serialport::Result<Box<dyn SerialPort>> {
    serialport::new(path, 9600)
        .data_bits(serialport::DataBits::Eight)
        .parity(serialport::Parity::None)
        .stop_bits(serialport::StopBits::One)
        .flow_control(serialport::FlowControl::None)
        .timeout(Duration::from_millis(100))
        .open()
}

fn read_forever(port: Option<String>) -> Result<(), String> {
    let path = dgtboard::pick_port(port).map_err(|e| format!("no board: {e}"))?;
    let mut port = open(&path).map_err(|e| format!("opening {path}: {e}"))?;
    // Reset → full board dump (replaces the preview) → stream field updates.
    port.write_all(&[cmd::SEND_RESET, cmd::SEND_BRD, cmd::SEND_UPDATE_BRD])
        .and_then(|_| port.flush())
        .map_err(|e| format!("writing to {path}: {e}"))?;
    eprintln!("dgt3d: reading board on {path}");

    let mut buf = [0u8; 256];
    loop {
        match port.read(&mut buf) {
            Ok(0) => {}
            Ok(n) => QUEUE.lock().unwrap().extend_from_slice(&buf[..n]),
            Err(ref e) if e.kind() == io::ErrorKind::TimedOut => {}
            Err(e) => return Err(format!("reading {path}: {e} (board unplugged?)")),
        }
    }
}
