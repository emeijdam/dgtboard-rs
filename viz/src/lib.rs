//! `dgtboard-bevy` — an "extreme wow" 3D viewer for a live DGT chess board.
//!
//! The whole scene is driven by [`dgtboard_core`]: a [`Decoder`] turns the
//! board's serial byte stream into board state, and a Bevy system diffs that
//! state against the 3D pieces every frame, spawning and despawning to match.
//! Pieces are **procedurally generated** lathe meshes (see [`pieces`]) — no
//! assets — lit with PBR materials, real shadows, and bloom.
//!
//! ## Transport
//!
//! Rust/wasm can't open a serial port itself, so the browser does: JavaScript
//! opens the Web Serial port, writes [`wasm_api::init_sequence`]'s bytes to start
//! the board, and pushes every received chunk into [`wasm_api::push_bytes`].
//! Those bytes land in a shared [`QUEUE`] that the Bevy `decode_system` drains.
//! Natively, `main.rs` calls [`run`], which seeds the queue with a synthetic
//! board dump so `cargo run --bin dgt3d` shows the start position to orbit.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use bevy::core_pipeline::bloom::BloomSettings;
use bevy::core_pipeline::tonemapping::Tonemapping;
use bevy::input::mouse::{MouseMotion, MouseWheel};
use bevy::prelude::*;

// `protocol`/`cmd` are each used on only one target (native dump vs wasm API).
#[allow(unused_imports)]
use dgtboard_core::protocol::{self, cmd};
use dgtboard_core::Color as PieceColor;
use dgtboard_core::{Board, Decoder, Piece};

mod pieces;
use pieces::{kind_index, lathe, profile, KINDS};

/// Bytes received from the board, pushed by JS (wasm) or seeded natively, and
/// drained by `decode_system`.
static QUEUE: Mutex<Vec<u8>> = Mutex::new(Vec::new());
/// Whether to read the board rotated 180° (White away from the cable).
static FLIP: AtomicBool = AtomicBool::new(false);
/// Set when the orientation changes: rebuild the decoder and clear the pieces.
static RESEED: AtomicBool = AtomicBool::new(false);

/// Native entry point (see `main.rs`). Seeds the queue with the start position
/// so the window opens onto a populated board, then runs the app.
#[cfg(not(target_arch = "wasm32"))]
pub fn run() {
    let dump = synthetic_board_dump(&Board::startpos());
    QUEUE.lock().unwrap().extend_from_slice(&dump);
    build_app().run();
}

/// The world position of a board square in DGT index order (0 = a8). File runs
/// along X, rank-row along Z, board top sits at y = 0.
fn square_world(idx: usize) -> Vec3 {
    let file = (idx % 8) as f32;
    let row = (idx / 8) as f32;
    Vec3::new(file - 3.5, 0.0, row - 3.5)
}

/// Live board state plus the square → piece-entity map the sync system diffs.
#[derive(Resource)]
struct BoardRes {
    decoder: Decoder,
    entities: [Option<(Entity, Piece)>; 64],
}

impl BoardRes {
    fn new() -> Self {
        BoardRes {
            decoder: Decoder::with_flip(FLIP.load(Ordering::Relaxed)),
            entities: [None; 64],
        }
    }
}

/// Shared mesh + material handles for the pieces, built once at startup.
#[derive(Resource)]
struct PieceAssets {
    /// One mesh per [`pieces::Kind`], indexed by `kind as usize`.
    meshes: [Handle<Mesh>; 6],
    white: Handle<StandardMaterial>,
    black: Handle<StandardMaterial>,
}

/// Manual orbit camera state, updated from the mouse each frame.
#[derive(Resource)]
struct Orbit {
    yaw: f32,
    pitch: f32,
    radius: f32,
    target: Vec3,
}

impl Default for Orbit {
    fn default() -> Self {
        Orbit {
            yaw: 0.7,
            pitch: 0.72,
            radius: 12.0,
            target: Vec3::new(0.0, 0.4, 0.0),
        }
    }
}

/// Construct the Bevy app (shared by the native and wasm entry points).
pub fn build_app() -> App {
    let mut app = App::new();
    app.add_plugins(DefaultPlugins.set(WindowPlugin {
        primary_window: Some(Window {
            title: "dgt3d — live DGT board".into(),
            // On wasm, render into the page's <canvas id="scene">.
            canvas: Some("#scene".into()),
            fit_canvas_to_parent: true,
            prevent_default_event_handling: false,
            ..default()
        }),
        ..default()
    }));

    app.insert_resource(ClearColor(Color::srgb(0.015, 0.016, 0.03)))
        .insert_resource(AmbientLight {
            color: Color::srgb(0.45, 0.5, 0.7),
            brightness: 55.0,
        })
        .insert_resource(Orbit::default())
        .insert_resource(BoardRes::new())
        .add_systems(Startup, setup)
        .add_systems(Update, (decode_system, sync_system, orbit_camera));

    app
}

/// Build the scene: camera (HDR + bloom + ACES), lights, ground, 64 tiles, and
/// the shared piece assets.
fn setup(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    // HDR camera with bloom and filmic tonemapping (AcesFitted needs no LUT).
    let orbit = Orbit::default();
    let cam_pos = orbit_position(&orbit);
    commands.spawn((
        Camera3dBundle {
            camera: Camera {
                hdr: true,
                ..default()
            },
            tonemapping: Tonemapping::AcesFitted,
            transform: Transform::from_translation(cam_pos).looking_at(orbit.target, Vec3::Y),
            ..default()
        },
        BloomSettings::NATURAL,
    ));

    // Key light: one crisp, shadow-casting directional light.
    commands.spawn(DirectionalLightBundle {
        directional_light: DirectionalLight {
            color: Color::srgb(1.0, 0.97, 0.9),
            illuminance: 9000.0,
            shadows_enabled: true,
            ..default()
        },
        transform: Transform::from_xyz(7.0, 14.0, 5.0).looking_at(Vec3::ZERO, Vec3::Y),
        ..default()
    });

    // Two coloured accent point lights, each with a small glowing orb so bloom
    // has something bright to bleed.
    let orb = meshes.add(Sphere::new(0.16));
    spawn_accent(
        &mut commands,
        &mut materials,
        orb.clone(),
        Vec3::new(-6.5, 3.2, -6.0),
        Color::srgb(0.25, 0.55, 1.0),
        LinearRgba::rgb(0.3, 1.4, 5.0),
    );
    spawn_accent(
        &mut commands,
        &mut materials,
        orb,
        Vec3::new(6.5, 2.6, 6.5),
        Color::srgb(1.0, 0.4, 0.55),
        LinearRgba::rgb(5.0, 0.7, 1.2),
    );

    // A large dark ground plane to catch the shadows.
    let ground_mat = materials.add(StandardMaterial {
        base_color: Color::srgb(0.03, 0.035, 0.05),
        perceptual_roughness: 0.85,
        metallic: 0.1,
        ..default()
    });
    commands.spawn(PbrBundle {
        mesh: meshes.add(Cuboid::new(120.0, 0.5, 120.0)),
        material: ground_mat,
        transform: Transform::from_xyz(0.0, -0.35, 0.0),
        ..default()
    });

    // 64 board tiles. a8 (idx 0) is a light square: (file + rank) even → light.
    let light_tile = materials.add(StandardMaterial {
        base_color: Color::srgb(0.86, 0.84, 0.78),
        perceptual_roughness: 0.35,
        metallic: 0.0,
        reflectance: 0.4,
        ..default()
    });
    let dark_tile = materials.add(StandardMaterial {
        base_color: Color::srgb(0.10, 0.13, 0.2),
        perceptual_roughness: 0.3,
        metallic: 0.2,
        reflectance: 0.5,
        ..default()
    });
    let tile_mesh = meshes.add(Cuboid::new(0.98, 0.2, 0.98));
    for idx in 0..64usize {
        let file = idx % 8;
        let rank = 8 - idx / 8; // human rank, idx 0 = a8
        let light = (file + rank) % 2 == 0;
        commands.spawn(PbrBundle {
            mesh: tile_mesh.clone(),
            material: if light {
                light_tile.clone()
            } else {
                dark_tile.clone()
            },
            // Tile top at y = 0: centre a half-thickness below.
            transform: Transform::from_translation(square_world(idx) - Vec3::Y * 0.1),
            ..default()
        });
    }

    // Glossy, marble-like piece materials — light base for White, dark for Black.
    let white = materials.add(StandardMaterial {
        base_color: Color::srgb(0.92, 0.90, 0.86),
        perceptual_roughness: 0.22,
        metallic: 0.05,
        reflectance: 0.6,
        double_sided: true,
        cull_mode: None,
        ..default()
    });
    let black = materials.add(StandardMaterial {
        base_color: Color::srgb(0.045, 0.05, 0.07),
        perceptual_roughness: 0.18,
        metallic: 0.25,
        reflectance: 0.6,
        double_sided: true,
        cull_mode: None,
        ..default()
    });

    // One lathe mesh per kind.
    let piece_meshes = KINDS.map(|k| meshes.add(lathe(&profile(k), 64)));

    commands.insert_resource(PieceAssets {
        meshes: piece_meshes,
        white,
        black,
    });
}

fn spawn_accent(
    commands: &mut Commands,
    materials: &mut Assets<StandardMaterial>,
    mesh: Handle<Mesh>,
    pos: Vec3,
    color: Color,
    emissive: LinearRgba,
) {
    commands.spawn(PointLightBundle {
        point_light: PointLight {
            color,
            intensity: 250_000.0,
            range: 40.0,
            radius: 0.16,
            shadows_enabled: false,
            ..default()
        },
        transform: Transform::from_translation(pos),
        ..default()
    });
    let glow = materials.add(StandardMaterial {
        base_color: Color::BLACK,
        emissive,
        ..default()
    });
    commands.spawn(PbrBundle {
        mesh,
        material: glow,
        transform: Transform::from_translation(pos),
        ..default()
    });
}

/// Drain the byte queue into the decoder. On a reseed (orientation change),
/// despawn every piece and rebuild the decoder first.
fn decode_system(mut commands: Commands, mut board: ResMut<BoardRes>) {
    if RESEED.swap(false, Ordering::Relaxed) {
        for slot in board.entities.iter_mut() {
            if let Some((e, _)) = slot.take() {
                commands.entity(e).despawn_recursive();
            }
        }
        board.decoder = Decoder::with_flip(FLIP.load(Ordering::Relaxed));
    }

    let bytes = std::mem::take(&mut *QUEUE.lock().unwrap());
    if bytes.is_empty() {
        return;
    }
    board.decoder.push(&bytes);
    // Drain every complete message; the decoder updates its own board state.
    while board.decoder.poll().is_some() {}
}

/// Diff the decoded board against the spawned pieces and reconcile (v1: pop in /
/// pop out; a future v2 could animate slides).
fn sync_system(
    mut commands: Commands,
    assets: Option<Res<PieceAssets>>,
    mut board: ResMut<BoardRes>,
) {
    let Some(assets) = assets else {
        return;
    };

    // Copy the 64 squares so we can mutate the entity map without aliasing.
    let squares = board.decoder.board().squares;
    for idx in 0..64usize {
        let want = squares[idx];
        let have = board.entities[idx].map(|(_, p)| p);
        if want == have {
            continue;
        }

        if let Some((e, _)) = board.entities[idx].take() {
            commands.entity(e).despawn_recursive();
        }
        if let Some(p) = want {
            let mesh = assets.meshes[kind_index(p)].clone();
            let material = if p.color() == PieceColor::White {
                assets.white.clone()
            } else {
                assets.black.clone()
            };
            let mut transform = Transform::from_translation(square_world(idx));
            // Spin black pieces around so any facing detail looks across the board.
            if p.color() == PieceColor::Black {
                transform.rotate_y(std::f32::consts::PI);
            }
            let entity = commands
                .spawn(PbrBundle {
                    mesh,
                    material,
                    transform,
                    ..default()
                })
                .id();
            board.entities[idx] = Some((entity, p));
        }
    }
}

/// Manual orbit camera: left-drag rotates yaw/pitch, scroll wheel zooms.
fn orbit_camera(
    buttons: Res<ButtonInput<MouseButton>>,
    mut motion: EventReader<MouseMotion>,
    mut wheel: EventReader<MouseWheel>,
    mut orbit: ResMut<Orbit>,
    mut cam: Query<&mut Transform, With<Camera>>,
) {
    let mut drag = Vec2::ZERO;
    for ev in motion.read() {
        drag += ev.delta;
    }
    let mut scroll = 0.0;
    for ev in wheel.read() {
        scroll += ev.y;
    }

    let mut changed = false;
    if buttons.pressed(MouseButton::Left) && drag != Vec2::ZERO {
        orbit.yaw -= drag.x * 0.005;
        orbit.pitch = (orbit.pitch - drag.y * 0.005).clamp(0.08, 1.52);
        changed = true;
    }
    if scroll != 0.0 {
        orbit.radius = (orbit.radius - scroll * 0.8).clamp(4.5, 30.0);
        changed = true;
    }
    if !changed {
        return;
    }

    let pos = orbit_position(&orbit);
    for mut t in &mut cam {
        *t = Transform::from_translation(pos).looking_at(orbit.target, Vec3::Y);
    }
}

/// Camera world position from spherical orbit coordinates.
fn orbit_position(orbit: &Orbit) -> Vec3 {
    let (sy, cy) = orbit.yaw.sin_cos();
    let (sp, cp) = orbit.pitch.sin_cos();
    orbit.target + Vec3::new(orbit.radius * cp * sy, orbit.radius * sp, orbit.radius * cp * cy)
}

/// The raw DGT byte code for a piece (inverse of `Piece::from_code`).
fn piece_code(p: Piece) -> u8 {
    use Piece::*;
    match p {
        WhitePawn => 0x01,
        WhiteRook => 0x02,
        WhiteKnight => 0x03,
        WhiteBishop => 0x04,
        WhiteKing => 0x05,
        WhiteQueen => 0x06,
        BlackPawn => 0x07,
        BlackRook => 0x08,
        BlackKnight => 0x09,
        BlackBishop => 0x0a,
        BlackKing => 0x0b,
        BlackQueen => 0x0c,
    }
}

/// A framed `BOARD_DUMP` message for `board`, as the hardware would send it.
/// Used to seed the native window with the start position.
#[cfg(not(target_arch = "wasm32"))]
fn synthetic_board_dump(board: &Board) -> Vec<u8> {
    let len = 3 + 64usize; // header + 64 piece codes
    let mut frame = Vec::with_capacity(len);
    frame.push(protocol::msg::BOARD_DUMP | protocol::MESSAGE_BIT);
    frame.push(((len >> 7) & 0x7f) as u8);
    frame.push((len & 0x7f) as u8);
    for sq in board.squares {
        frame.push(sq.map(piece_code).unwrap_or(0));
    }
    frame
}

/// The browser-facing API. JS opens the Web Serial port, then drives these.
#[cfg(target_arch = "wasm32")]
pub mod wasm_api {
    use super::*;
    use wasm_bindgen::prelude::*;

    /// Install a readable panic hook, store the orientation, and run the app.
    /// Never returns (the winit event loop takes over).
    #[wasm_bindgen]
    pub fn start(flip: bool) {
        console_error_panic_hook::set_once();
        FLIP.store(flip, Ordering::Relaxed);
        build_app().run();
    }

    /// Append a chunk of bytes received from the board to the shared queue.
    #[wasm_bindgen]
    pub fn push_bytes(bytes: &[u8]) {
        QUEUE.lock().unwrap().extend_from_slice(bytes);
    }

    /// Change the board orientation live and trigger a reseed (the page should
    /// re-send `init_sequence` so the position re-dumps).
    #[wasm_bindgen]
    pub fn set_flip(flip: bool) {
        FLIP.store(flip, Ordering::Relaxed);
        RESEED.store(true, Ordering::Relaxed);
    }

    /// The bytes to write to the board to begin: reset, request a full dump,
    /// then stream field updates.
    #[wasm_bindgen]
    pub fn init_sequence() -> Vec<u8> {
        vec![cmd::SEND_RESET, cmd::SEND_BRD, cmd::SEND_UPDATE_BRD]
    }
}
