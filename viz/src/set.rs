//! The scanned piece set: Poly Haven's CC0 "Chess Set" (Riley Queen), loaded
//! from glTF at `assets/chess_set/`.
//!
//! The file has one node per piece (`piece_king_white`, `piece_knight_black_01`,
//! …) plus a `board` node, with three PBR materials (white pieces, black
//! pieces, board). We resolve one representative node per [`Kind`] — the white
//! geometry, which both colours share — measure its mesh and build a local
//! [`Transform`] that puts the base centre on the origin and rescales the
//! model's ~0.058-unit square pitch to our one-unit squares. Black pieces are
//! the same meshes with the black material.
//!
//! Loading is asynchronous: until the glTF resolves, the scene uses the
//! procedural lathe pieces from [`pieces`](super::pieces) and 64 flat tiles;
//! when it resolves, the pieces and tiles are swapped for the scanned ones.
//! If the assets are missing or broken, the lathe set simply stays.

use bevy::asset::LoadState;
use bevy::gltf::{Gltf, GltfMesh, GltfNode};
use bevy::prelude::*;

use super::pieces::{Kind, KINDS};
use super::{square_world, BoardRes, Tile};

/// Distance between square centres in the glTF's own units; the inverse is
/// the scale that makes one square one world unit.
pub const SQUARE_PITCH: f32 = 0.058;

/// Path of the glTF relative to the asset root.
pub const GLTF_PATH: &str = "chess_set/chess_set_2k.gltf";

/// A resolved piece: its mesh primitives (bishops have two) and the local
/// transform that centres its base on the origin in board units.
pub struct SetPiece {
    pub primitives: Vec<Handle<Mesh>>,
    pub local: Transform,
}

/// The resolved set, ready to spawn. Present only once the glTF has loaded.
#[derive(Resource)]
pub struct PieceSet {
    /// One entry per [`Kind`], indexed by `kind as usize`.
    pub pieces: [SetPiece; 6],
    pub white: Handle<StandardMaterial>,
    pub black: Handle<StandardMaterial>,
}

/// The glTF handle while it loads; removed once resolved or failed.
#[derive(Resource)]
pub(super) struct Loading(Handle<Gltf>);

/// Kick off the glTF load. Runs at startup.
pub fn start_loading(mut commands: Commands, asset_server: Res<AssetServer>) {
    commands.insert_resource(Loading(asset_server.load::<Gltf>(GLTF_PATH)));
}

/// The directory the asset server reads from. Natively, prefer the crate's
/// own `assets/` directory (so `./target/debug/dgt3d` works from anywhere
/// on a dev machine); otherwise Bevy's default `assets` next to the
/// executable / page.
pub fn asset_root() -> String {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let dev = concat!(env!("CARGO_MANIFEST_DIR"), "/assets");
        if std::path::Path::new(dev).is_dir() {
            return dev.to_string();
        }
    }
    "assets".to_string()
}

/// Representative glTF node for each kind (the white piece; both colours
/// share geometry).
fn node_name(kind: Kind) -> &'static str {
    match kind {
        Kind::Pawn => "piece_pawn_white_01",
        Kind::Knight => "piece_knight_white_01",
        Kind::Bishop => "piece_bishop_white_01",
        Kind::Rook => "piece_rook_white_01",
        Kind::Queen => "piece_queen_white",
        Kind::King => "piece_king_white",
    }
}

/// A node's primitives, first material, and the union AABB of its meshes.
struct Resolved {
    primitives: Vec<Handle<Mesh>>,
    material: Option<Handle<StandardMaterial>>,
    min: Vec3,
    max: Vec3,
}

fn resolve_node(
    gltf: &Gltf,
    name: &str,
    nodes: &Assets<GltfNode>,
    gltf_meshes: &Assets<GltfMesh>,
    meshes: &Assets<Mesh>,
) -> Option<Resolved> {
    let node = nodes.get(gltf.named_nodes.get(name)?)?;
    let gm = gltf_meshes.get(node.mesh.as_ref()?)?;
    let mut out = Resolved {
        primitives: Vec::new(),
        material: gm.primitives.first()?.material.clone(),
        min: Vec3::splat(f32::INFINITY),
        max: Vec3::splat(f32::NEG_INFINITY),
    };
    for prim in &gm.primitives {
        let aabb = meshes.get(&prim.mesh)?.compute_aabb()?;
        out.min = out.min.min(Vec3::from(aabb.min()));
        out.max = out.max.max(Vec3::from(aabb.max()));
        out.primitives.push(prim.mesh.clone());
    }
    Some(out)
}

/// Local transform that moves the AABB's base centre to the origin and
/// rescales to board units. Child points map as `T + S·p`, so the
/// translation is the (scaled) negative of the base centre.
fn base_centred(r: &Resolved) -> Transform {
    let s = 1.0 / SQUARE_PITCH;
    let base = Vec3::new((r.min.x + r.max.x) * 0.5, r.min.y, (r.min.z + r.max.z) * 0.5);
    Transform {
        translation: -base * s,
        scale: Vec3::splat(s),
        ..default()
    }
}

/// Once the glTF has loaded, resolve the pieces and board, swap the tiles for
/// the scanned board, and clear the spawned pieces so `sync_system` respawns
/// them from the set. On failure, log once and keep the lathe set.
#[allow(clippy::too_many_arguments)]
pub fn resolve_set(
    mut commands: Commands,
    loading: Option<Res<Loading>>,
    asset_server: Res<AssetServer>,
    gltfs: Res<Assets<Gltf>>,
    nodes: Res<Assets<GltfNode>>,
    gltf_meshes: Res<Assets<GltfMesh>>,
    meshes: Res<Assets<Mesh>>,
    mut board: ResMut<BoardRes>,
    tiles: Query<Entity, With<Tile>>,
) {
    let Some(loading) = loading else {
        return;
    };
    match asset_server.load_state(&loading.0) {
        LoadState::Loaded => {}
        LoadState::Failed(err) => {
            warn!("scanned piece set unavailable ({GLTF_PATH}): {err}; using procedural pieces");
            commands.remove_resource::<Loading>();
            return;
        }
        _ => return,
    }
    let Some(gltf) = gltfs.get(&loading.0) else {
        return;
    };

    let resolved: Option<Vec<Resolved>> = KINDS
        .iter()
        .map(|&k| resolve_node(gltf, node_name(k), &nodes, &gltf_meshes, &meshes))
        .collect();
    let (Some(resolved), Some(white), Some(black)) = (
        resolved,
        gltf.named_materials.get("chess_set_pieces_white").cloned(),
        gltf.named_materials.get("chess_set_pieces_black").cloned(),
    ) else {
        warn!("{GLTF_PATH} doesn't have the expected piece nodes/materials; using procedural pieces");
        commands.remove_resource::<Loading>();
        return;
    };
    let pieces: [SetPiece; 6] = resolved
        .iter()
        .map(|r| SetPiece {
            primitives: r.primitives.clone(),
            local: base_centred(r),
        })
        .collect::<Vec<_>>()
        .try_into()
        .ok()
        .expect("six kinds");

    // The scanned board replaces the flat tiles: centred, its top face at y = 0.
    if let Some(b) = resolve_node(gltf, "board", &nodes, &gltf_meshes, &meshes) {
        let s = 1.0 / SQUARE_PITCH;
        let centre = Vec3::new((b.min.x + b.max.x) * 0.5, b.max.y, (b.min.z + b.max.z) * 0.5);
        let transform = Transform {
            translation: -centre * s,
            scale: Vec3::splat(s),
            ..default()
        };
        if let Some(material) = b.material.clone() {
            for e in &tiles {
                commands.entity(e).despawn_recursive();
            }
            for mesh in b.primitives {
                commands.spawn(PbrBundle {
                    mesh,
                    material: material.clone(),
                    transform,
                    ..default()
                });
            }
        }
    }

    // Drop the lathe pieces; sync_system respawns every square from the set.
    for slot in board.entities.iter_mut() {
        if let Some((e, _)) = slot.take() {
            commands.entity(e).despawn_recursive();
        }
    }

    commands.insert_resource(PieceSet { pieces, white, black });
    commands.remove_resource::<Loading>();
    info!("scanned piece set loaded from {GLTF_PATH}");
}

/// Spawn a scanned piece of `kind` at `square` (DGT index) and return its
/// root entity. The scan's knights look along +Z — towards White's edge of
/// our board — so White's pieces get a half turn to face Black, and Black's
/// stand as scanned.
pub fn spawn_piece(
    commands: &mut Commands,
    set: &PieceSet,
    kind_idx: usize,
    black: bool,
    square: usize,
) -> Entity {
    let piece = &set.pieces[kind_idx];
    let material = if black { set.black.clone() } else { set.white.clone() };
    let mut root = Transform::from_translation(square_world(square));
    if !black {
        root.rotate_y(std::f32::consts::PI);
    }
    commands
        .spawn(SpatialBundle::from_transform(root))
        .with_children(|parent| {
            for mesh in &piece.primitives {
                parent.spawn(PbrBundle {
                    mesh: mesh.clone(),
                    material: material.clone(),
                    transform: piece.local,
                    ..default()
                });
            }
        })
        .id()
}
