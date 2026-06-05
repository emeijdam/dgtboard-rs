//! Raw byte constants for the DGT electronic board serial protocol.
//!
//! Source: the DGT Electronic Board Protocol Description, as captured in the
//! long-circulated `dgtbrd*.h` headers used by PicoChess and others.

/// Set on byte 0 of every message the board sends back to the host.
pub const MESSAGE_BIT: u8 = 0x80;

/// One-byte commands the host sends to the board.
pub mod cmd {
    /// Put the board into idle mode; it then stays silent until queried.
    pub const SEND_RESET: u8 = 0x40;
    /// Request a one-shot full board dump (`BOARD_DUMP` reply).
    pub const SEND_BRD: u8 = 0x42;
    /// Enter update mode: board streams field updates + clock messages.
    pub const SEND_UPDATE_BRD: u8 = 0x44;
    /// Like `SEND_UPDATE_BRD` but only emits clock messages when they change.
    pub const SEND_UPDATE_NICE: u8 = 0x4b;
    /// Ask for the firmware version (`VERSION` reply).
    pub const SEND_VERSION: u8 = 0x4d;
    /// Ask for the short serial number (`SERIALNR` reply).
    pub const RETURN_SERIALNR: u8 = 0x45;
}

/// Message identifiers the board sends back (with [`MESSAGE_BIT`] already stripped).
pub mod msg {
    /// 64 bytes of piece codes, square a8..h1.
    pub const BOARD_DUMP: u8 = 0x06;
    /// Clock / button time message.
    pub const BWTIME: u8 = 0x0d;
    /// A single square changed: `[field, piece_code]`.
    pub const FIELD_UPDATE: u8 = 0x0e;
    /// Short serial number (ASCII).
    pub const SERIALNR: u8 = 0x11;
    /// Firmware version: `[major, minor]`.
    pub const VERSION: u8 = 0x13;
}
