//! `dgtboard-bevy` — an "extreme wow" 3D viewer for a live DGT chess board.
//!
//! The whole scene is driven by [`dgtboard_core`]: a [`Decoder`] turns the
//! board's serial byte stream into board state, and a Bevy system diffs that
//! state against the 3D pieces every frame, spawning and despawning to match.
//! Pieces are Poly Haven's photoscanned marble chess set (CC0; see [`set`]),
//! lit with PBR materials, real shadows, and bloom. While that glTF loads — or
//! if it's missing — procedurally generated lathe pieces (see [`pieces`]) and
//! flat tiles stand in.
//!
//! Over the board sits an [`overlay`]: square marks, a ghost of the piece the
//! opponent just moved, and a HUD — plain data in the [`Overlay`] resource
//! that a game session fills. `Tab` swaps the 3D scene for a flat
//! Lichess-style board (see [`flat`]) showing the same position and overlay;
//! `F` views either from Black's side (the [`View`] resource).
//!
//! ## Transport
//!
//! Rust/wasm can't open a serial port itself, so the browser does: JavaScript
//! opens the Web Serial port, writes [`wasm_api::init_sequence`]'s bytes to start
//! the board, and pushes every received chunk into [`wasm_api::push_bytes`].
//! Those bytes land in a shared [`QUEUE`] that the Bevy `decode_system` drains.
//! Natively, `main.rs` calls [`run`], which seeds the queue with a synthetic
//! board dump so `cargo run --bin dgt3d` shows the start position at once,
//! then a reader thread (see [`serial`]) opens the real board and streams its
//! bytes into the same queue.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use bevy::asset::AssetMetaCheck;
use bevy::core_pipeline::bloom::BloomSettings;
use bevy::core_pipeline::tonemapping::Tonemapping;
use bevy::input::mouse::{MouseMotion, MouseScrollUnit, MouseWheel};
use bevy::prelude::*;

// `protocol`/`cmd` are each used on only one target (native dump vs wasm API).
#[allow(unused_imports)]
use dgtboard_core::protocol::{self, cmd};
use dgtboard_core::Color as PieceColor;
use dgtboard_core::{Board, Decoder, Piece};

mod flat;
pub mod overlay;
mod pieces;
mod referee;
#[cfg(not(target_arch = "wasm32"))]
mod serial;
#[cfg(not(target_arch = "wasm32"))]
pub mod session;
mod set;
pub use overlay::{post_overlay, post_view, Clocks, Hud, Mark, Overlay, View};
use pieces::{kind_index, lathe, profile, KINDS};
use set::PieceSet;

/// Bytes received from the board, pushed by JS (wasm) or seeded natively, and
/// drained by `decode_system`.
static QUEUE: Mutex<Vec<u8>> = Mutex::new(Vec::new());
/// Whether to read the board rotated 180° (White away from the cable).
static FLIP: AtomicBool = AtomicBool::new(false);
/// Set when the orientation changes: rebuild the decoder and clear the pieces.
static RESEED: AtomicBool = AtomicBool::new(false);

/// Native launch options (see `main.rs`).
#[cfg(not(target_arch = "wasm32"))]
#[derive(Default, Clone, Debug)]
pub struct Options {
    /// How to reach the board (and Lichess).
    pub connection: session::Connection,
    /// A Lichess game or puzzle session to run in the window; `None` just
    /// follows the board.
    pub mode: Option<session::Mode>,
}

/// Native entry point (see `main.rs`). Seeds the queue with the start position
/// so the window opens onto a populated board, then either starts the serial
/// reader or a Lichess / puzzle session (which owns the board itself), and
/// runs the app.
#[cfg(not(target_arch = "wasm32"))]
pub fn run(options: Options) {
    FLIP.store(options.connection.flip, Ordering::Relaxed);
    preview_seed();
    let mut app = build_app();
    match options.mode {
        Some(mode) => {
            let handle = session::spawn(mode, options.connection);
            app.insert_resource(session::SessionRes::new(handle))
                .add_systems(Update, (session::notices, session::keys).before(overlay::apply_posted));
        }
        None => {
            serial::spawn_reader(options.connection.port);
            add_referee(&mut app);
        }
    }
    app.run();
}

/// Free play: the local referee draws the overlay from the board alone.
fn add_referee(app: &mut App) {
    app.init_resource::<referee::Referee>()
        .add_systems(
            Update,
            referee::system.after(decode_system).before(overlay::sync_marks_3d),
        );
}

/// Seed the queue with a start-position board dump so the scene shows pieces
/// before a real board is connected. Honours the current [`FLIP`] so the
/// preview always decodes to the standard start position. A real board's dump
/// (sent in response to `init_sequence`) simply replaces it on connect.
fn preview_seed() {
    let board = if FLIP.load(Ordering::Relaxed) {
        // The decoder will rotate the dump by 180° when flipped; pre-rotate so
        // the result is still the standard start position.
        Board::startpos().rotated_180()
    } else {
        Board::startpos()
    };
    QUEUE
        .lock()
        .unwrap()
        .extend_from_slice(&synthetic_board_dump(&board));
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

/// Shared mesh + material handles for the fallback lathe pieces, built once at
/// startup.
#[derive(Resource)]
struct PieceAssets {
    /// One mesh per [`pieces::Kind`], indexed by `kind as usize`.
    meshes: [Handle<Mesh>; 6],
    white: Handle<StandardMaterial>,
    black: Handle<StandardMaterial>,
}

/// Marker for the flat fallback tiles (replaced by the scanned board).
#[derive(Component)]
struct Tile;

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
    app.add_plugins(
        DefaultPlugins
            .set(WindowPlugin {
                primary_window: Some(Window {
                    title: "dgt3d — live DGT board".into(),
                    // On wasm, render into the page's <canvas id="scene">.
                    canvas: Some("#scene".into()),
                    fit_canvas_to_parent: true,
                    prevent_default_event_handling: false,
                    ..default()
                }),
                ..default()
            })
            .set(AssetPlugin {
                file_path: set::asset_root(),
                // No `.meta` sidecars ship with the assets; skip probing for
                // them (on wasm each probe is a 404 round trip).
                meta_check: AssetMetaCheck::Never,
                ..default()
            }),
    );

    app.insert_resource(ClearColor(Color::srgb(0.015, 0.016, 0.03)))
        .insert_resource(AmbientLight {
            color: Color::srgb(0.62, 0.64, 0.72),
            brightness: 120.0,
        })
        .insert_resource(Orbit::default())
        .insert_resource(BoardRes::new())
        .init_resource::<Overlay>()
        .init_resource::<View>()
        .add_systems(
            Startup,
            (
                setup,
                set::start_loading,
                overlay::setup_3d,
                overlay::setup_hud,
                flat::setup,
            ),
        )
        .add_systems(
            Update,
            (
                overlay::apply_posted,
                overlay::view_keys,
                overlay::apply_view,
                decode_system,
                set::resolve_set,
                sync_system,
                orbit_camera,
                overlay::sync_marks_3d,
                overlay::sync_ghost_3d,
                overlay::sync_hud,
                flat::layout,
                flat::sync_pieces,
                flat::sync_overlay,
            )
                .chain(),
        );

    app
}

/// Build the scene: camera (HDR + bloom + ACES), lights, ground, 64 tiles, and
/// the shared fallback piece assets.
fn setup(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    // HDR camera with bloom and filmic tonemapping (AcesFitted needs no LUT).
    let orbit = Orbit::default();
    let cam_pos = orbit_position(&orbit, false);
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
            illuminance: 11500.0,
            shadows_enabled: true,
            ..default()
        },
        transform: Transform::from_xyz(7.0, 14.0, 5.0).looking_at(Vec3::ZERO, Vec3::Y),
        ..default()
    });

    // Soft white fill from the camera/front side (a directional light gives even
    // illuminance regardless of distance) so the faces we look at read true to
    // their colour — bright White, dark matte Black. No shadows, just relief.
    commands.spawn(DirectionalLightBundle {
        directional_light: DirectionalLight {
            color: Color::srgb(0.92, 0.95, 1.0),
            illuminance: 4500.0,
            shadows_enabled: false,
            ..default()
        },
        transform: Transform::from_xyz(-3.0, 7.0, 11.0).looking_at(Vec3::ZERO, Vec3::Y),
        ..default()
    });

    // Two coloured accent point lights, each with a small glowing orb so bloom
    // has something bright to bleed. Kept gentle so the marble reads as marble.
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

    // 64 fallback board tiles. a8 (idx 0) is a light square: (file + rank)
    // even → light. Replaced by the scanned board once the set loads.
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
        commands.spawn((
            PbrBundle {
                mesh: tile_mesh.clone(),
                material: if light {
                    light_tile.clone()
                } else {
                    dark_tile.clone()
                },
                // Tile top at y = 0: centre a half-thickness below.
                transform: Transform::from_translation(square_world(idx) - Vec3::Y * 0.1),
                ..default()
            },
            Tile,
        ));
    }

    // Glossy, marble-like fallback piece materials — light base for White,
    // dark for Black.
    let white = materials.add(StandardMaterial {
        base_color: Color::srgb(0.94, 0.92, 0.88),
        perceptual_roughness: 0.34,
        metallic: 0.05,
        reflectance: 0.55,
        // A gentle self-lit floor so ivory pieces always read clearly lighter
        // than the matte-black set, even on faces the key light misses.
        emissive: LinearRgba::rgb(0.16, 0.16, 0.2),
        double_sided: true,
        cull_mode: None,
        ..default()
    });
    let black = materials.add(StandardMaterial {
        base_color: Color::srgb(0.02, 0.022, 0.03),
        perceptual_roughness: 0.3,
        metallic: 0.0,
        reflectance: 0.35,
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
            intensity: 120_000.0,
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
/// pop out; a future v2 could animate slides). Uses the scanned set once it has
/// loaded, the lathe pieces until then.
fn sync_system(
    mut commands: Commands,
    assets: Option<Res<PieceAssets>>,
    set: Option<Res<PieceSet>>,
    mut board: ResMut<BoardRes>,
) {
    let Some(assets) = assets else {
        return;
    };

    // Copy the 64 squares so we can mutate the entity map without aliasing.
    let squares = board.decoder.board().squares;
    for (idx, &want) in squares.iter().enumerate() {
        let have = board.entities[idx].map(|(_, p)| p);
        if want == have {
            continue;
        }

        if let Some((e, _)) = board.entities[idx].take() {
            commands.entity(e).despawn_recursive();
        }
        if let Some(p) = want {
            let black = p.color() == PieceColor::Black;
            let entity = match set.as_deref() {
                Some(set) => set::spawn_piece(&mut commands, set, kind_index(p), black, idx),
                None => spawn_lathe_piece(&mut commands, &assets, p, black, idx),
            };
            board.entities[idx] = Some((entity, p));
        }
    }
}

/// Spawn a fallback lathe piece at `idx`.
fn spawn_lathe_piece(
    commands: &mut Commands,
    assets: &PieceAssets,
    p: Piece,
    black: bool,
    idx: usize,
) -> Entity {
    let material = if black {
        assets.black.clone()
    } else {
        assets.white.clone()
    };
    spawn_lathe_piece_with(commands, assets, p, black, idx, material)
}

/// [`spawn_lathe_piece`] with an explicit material (the overlay's ghost).
fn spawn_lathe_piece_with(
    commands: &mut Commands,
    assets: &PieceAssets,
    p: Piece,
    black: bool,
    idx: usize,
    material: Handle<StandardMaterial>,
) -> Entity {
    let mesh = assets.meshes[kind_index(p)].clone();
    let mut transform = Transform::from_translation(square_world(idx));
    // Spin black pieces around so any facing detail looks across the board.
    if black {
        transform.rotate_y(std::f32::consts::PI);
    }
    commands
        .spawn(PbrBundle {
            mesh,
            material,
            transform,
            ..default()
        })
        .id()
}

/// Manual orbit camera: left-drag rotates yaw/pitch, scroll wheel zooms.
/// Viewing from Black adds a half turn of yaw, so the same drag feels the
/// same from either side.
fn orbit_camera(
    buttons: Res<ButtonInput<MouseButton>>,
    mut motion: EventReader<MouseMotion>,
    mut wheel: EventReader<MouseWheel>,
    view: Res<View>,
    mut orbit: ResMut<Orbit>,
    mut cam: Query<&mut Transform, With<Camera3d>>,
) {
    let mut drag = Vec2::ZERO;
    for ev in motion.read() {
        drag += ev.delta;
    }
    // Browsers report wheel deltas in pixels (dozens per notch) where native
    // reports lines; normalise so one notch zooms about the same everywhere.
    let mut scroll = 0.0;
    for ev in wheel.read() {
        scroll += match ev.unit {
            MouseScrollUnit::Line => ev.y,
            MouseScrollUnit::Pixel => ev.y / 40.0,
        };
    }

    let mut changed = view.is_changed();
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

    let pos = orbit_position(&orbit, view.from_black);
    for mut t in &mut cam {
        *t = Transform::from_translation(pos).looking_at(orbit.target, Vec3::Y);
    }
}

/// Camera world position from spherical orbit coordinates, turned half way
/// round the board when viewing from Black.
fn orbit_position(orbit: &Orbit, from_black: bool) -> Vec3 {
    let yaw = if from_black {
        orbit.yaw + std::f32::consts::PI
    } else {
        orbit.yaw
    };
    let (sy, cy) = yaw.sin_cos();
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
/// Used to seed the scene with a preview start position (see [`preview_seed`]).
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
        // Show the start position immediately, before any board is connected.
        preview_seed();
        let mut app = build_app();
        add_referee(&mut app);
        app.run();
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
        // Keep the preview populated after a flip while disconnected; if a board
        // is connected, its fresh dump replaces this moments later.
        preview_seed();
    }

    /// The bytes to write to the board to begin: reset, request a full dump,
    /// then stream field updates.
    #[wasm_bindgen]
    pub fn init_sequence() -> Vec<u8> {
        vec![cmd::SEND_RESET, cmd::SEND_BRD, cmd::SEND_UPDATE_BRD]
    }

    /// Show the flat 2D board (`true`) or the 3D scene (`false`).
    #[wasm_bindgen]
    pub fn set_view(two_d: bool) {
        post_view(Some(two_d), None);
    }

    /// View the board from Black's side (`true`) or White's (`false`).
    #[wasm_bindgen]
    pub fn set_side(black: bool) {
        post_view(None, Some(black));
    }

    /// Fill the overlay with a canned mid-game state (see [`overlay::demo`])
    /// so the marks, ghost and HUD can be eyeballed without a game.
    #[wasm_bindgen]
    pub fn demo_overlay() {
        post_overlay(overlay::demo());
    }
}
