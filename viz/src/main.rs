//! Native launcher for the 3D viewer.
//!
//! `cargo run -p dgtboard-bevy --bin dgt3d` opens a window seeded with the start
//! position and, if a DGT board is plugged in, follows it live. Orbit with
//! left-drag, zoom with the scroll wheel.

#[cfg(not(target_arch = "wasm32"))]
use clap::Parser;

#[cfg(not(target_arch = "wasm32"))]
#[derive(Parser)]
#[command(name = "dgt3d", about = "A live DGT chess board as a real-time 3D scene")]
struct Args {
    /// Serial port path. Auto-detected if omitted.
    #[arg(short, long)]
    port: Option<String>,
    /// Read the board rotated 180° (White at the end away from the cable).
    #[arg(short, long)]
    flip: bool,
}

#[cfg(not(target_arch = "wasm32"))]
fn main() {
    let args = Args::parse();
    dgtboard_bevy::run(dgtboard_bevy::Options {
        port: args.port,
        flip: args.flip,
    });
}

/// The browser build is the cdylib (see `wasm_api`); this bin has no role there.
#[cfg(target_arch = "wasm32")]
fn main() {}
