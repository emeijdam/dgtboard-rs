//! Native launcher for the 3D viewer.
//!
//! `cargo run -p dgtboard-bevy --bin dgt3d` opens a window seeded with the start
//! position — orbit it with left-drag, zoom with the scroll wheel. To drive it
//! from a real board, use the wasm build and the `web3d/` Web Serial demo.

fn main() {
    dgtboard_bevy::run();
}
