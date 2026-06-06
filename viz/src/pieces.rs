//! Procedural chess pieces — zero assets.
//!
//! Every piece is a **surface of revolution** (a "lathe"): a 2D silhouette of
//! `(radius, y)` points spun around the vertical axis. We build the POSITION /
//! NORMAL / UV0 attributes by hand and compute normals analytically by rotating
//! the 2D profile normal, so the meshes are smooth with no asset pipeline.
//!
//! Pawn / rook / bishop / queen / king get classic Staunton silhouettes. A real
//! knight is *not* rotationally symmetric, so it can't be a true lathe — we give
//! it a distinct, stylized turned shape instead (a CC0 GLTF knight could replace
//! it later; see the README).

use bevy::prelude::*;
use bevy::render::mesh::Indices;
use bevy::render::render_asset::RenderAssetUsages;
use bevy::render::render_resource::PrimitiveTopology;
use dgtboard_core::Piece;

/// The six distinct piece silhouettes.
#[derive(Clone, Copy)]
pub enum Kind {
    Pawn,
    Knight,
    Bishop,
    Rook,
    Queen,
    King,
}

/// In declared (discriminant) order, so `meshes[kind as usize]` lines up.
pub const KINDS: [Kind; 6] = [
    Kind::Pawn,
    Kind::Knight,
    Kind::Bishop,
    Kind::Rook,
    Kind::Queen,
    Kind::King,
];

/// Which silhouette a board piece uses (colour is handled by the material).
pub fn kind_of(p: Piece) -> Kind {
    use Piece::*;
    match p {
        WhitePawn | BlackPawn => Kind::Pawn,
        WhiteKnight | BlackKnight => Kind::Knight,
        WhiteBishop | BlackBishop => Kind::Bishop,
        WhiteRook | BlackRook => Kind::Rook,
        WhiteQueen | BlackQueen => Kind::Queen,
        WhiteKing | BlackKing => Kind::King,
    }
}

/// Index into the per-kind mesh table.
pub fn kind_index(p: Piece) -> usize {
    kind_of(p) as usize
}

/// Build a mesh by revolving `profile` (a list of `(radius, y)` points, bottom
/// to top) around the Y axis with `segments` angular subdivisions.
///
/// Normals are derived from the profile's own 2D normal then rotated into 3D, so
/// curved sections shade smoothly. Pair the result with a `double_sided`,
/// `cull_mode: None` material so triangle winding never matters.
pub fn lathe(profile: &[Vec2], segments: usize) -> Mesh {
    let rings = profile.len();
    assert!(rings >= 2 && segments >= 3, "degenerate lathe");

    // Per-profile-point 2D normal in the (radius, y) plane: average of the two
    // adjacent edge normals. Edge tangent t = next - prev; outward normal is
    // (t.y, -t.x), which points to increasing radius for an upward profile.
    let mut pn = vec![Vec2::ZERO; rings];
    for i in 0..rings {
        let mut n = Vec2::ZERO;
        if i + 1 < rings {
            let d = profile[i + 1] - profile[i];
            n += Vec2::new(d.y, -d.x);
        }
        if i > 0 {
            let d = profile[i] - profile[i - 1];
            n += Vec2::new(d.y, -d.x);
        }
        pn[i] = n.normalize_or_zero();
    }

    let vcount = rings * (segments + 1);
    let mut positions = Vec::with_capacity(vcount);
    let mut normals = Vec::with_capacity(vcount);
    let mut uvs = Vec::with_capacity(vcount);

    for s in 0..=segments {
        let u = s as f32 / segments as f32;
        let theta = u * std::f32::consts::TAU;
        let (sin, cos) = theta.sin_cos();
        for i in 0..rings {
            let r = profile[i].x;
            let y = profile[i].y;
            positions.push([r * cos, y, r * sin]);

            // Rotate the 2D profile normal (radial, vertical) into 3D.
            let nr = pn[i].x;
            let ny = pn[i].y;
            let mut nor = Vec3::new(nr * cos, ny, nr * sin);
            if nor.length_squared() < 1e-8 {
                nor = Vec3::Y;
            }
            nor = nor.normalize();
            normals.push([nor.x, nor.y, nor.z]);
            uvs.push([u, i as f32 / (rings - 1) as f32]);
        }
    }

    let mut indices = Vec::with_capacity(segments * (rings - 1) * 6);
    for s in 0..segments {
        for i in 0..rings - 1 {
            let a = (s * rings + i) as u32;
            let b = (s * rings + i + 1) as u32;
            let c = ((s + 1) * rings + i) as u32;
            let d = ((s + 1) * rings + i + 1) as u32;
            indices.extend_from_slice(&[a, c, b, b, c, d]);
        }
    }

    let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default());
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uvs);
    mesh.insert_indices(Indices::U32(indices));
    mesh
}

#[inline]
fn v(r: f32, y: f32) -> Vec2 {
    Vec2::new(r, y)
}

/// The silhouette for a piece kind. Each profile starts at the base centre
/// `(0, 0)` and ends at the top centre `(0, height)`, so the solid is closed top
/// and bottom. One world unit = one board square; pieces stay under ~0.4 wide.
pub fn profile(kind: Kind) -> Vec<Vec2> {
    match kind {
        Kind::Pawn => vec![
            v(0.00, 0.00),
            v(0.30, 0.00),
            v(0.30, 0.04),
            v(0.20, 0.09),
            v(0.13, 0.16),
            v(0.16, 0.21),
            v(0.12, 0.30),
            v(0.10, 0.36),
            v(0.19, 0.41),
            v(0.205, 0.47),
            v(0.185, 0.54),
            v(0.12, 0.585),
            v(0.00, 0.60),
        ],
        Kind::Rook => vec![
            v(0.00, 0.00),
            v(0.34, 0.00),
            v(0.34, 0.05),
            v(0.23, 0.11),
            v(0.20, 0.30),
            v(0.215, 0.46),
            v(0.255, 0.52),
            v(0.28, 0.56),
            v(0.28, 0.66),
            // hollow crown lip
            v(0.20, 0.66),
            v(0.20, 0.60),
            v(0.00, 0.60),
        ],
        Kind::Bishop => vec![
            v(0.00, 0.00),
            v(0.31, 0.00),
            v(0.31, 0.05),
            v(0.20, 0.11),
            v(0.135, 0.20),
            v(0.155, 0.30),
            v(0.115, 0.40),
            v(0.20, 0.46),
            v(0.155, 0.50),
            v(0.10, 0.58),
            v(0.155, 0.66),
            v(0.175, 0.74),
            v(0.13, 0.82),
            v(0.055, 0.88),
            v(0.075, 0.93),
            v(0.00, 0.97),
        ],
        Kind::Queen => vec![
            v(0.00, 0.00),
            v(0.35, 0.00),
            v(0.35, 0.05),
            v(0.23, 0.12),
            v(0.155, 0.26),
            v(0.13, 0.44),
            v(0.16, 0.58),
            v(0.115, 0.66),
            v(0.235, 0.74),
            // collar then crown points (single ring of spikes via a flare)
            v(0.255, 0.80),
            v(0.20, 0.86),
            v(0.255, 0.92),
            v(0.18, 0.98),
            v(0.09, 1.00),
            v(0.13, 1.06),
            v(0.00, 1.10),
        ],
        Kind::King => vec![
            v(0.00, 0.00),
            v(0.36, 0.00),
            v(0.36, 0.05),
            v(0.24, 0.12),
            v(0.16, 0.28),
            v(0.135, 0.48),
            v(0.165, 0.62),
            v(0.12, 0.70),
            v(0.24, 0.78),
            v(0.255, 0.84),
            v(0.21, 0.90),
            v(0.155, 0.96),
            v(0.18, 1.02),
            // cross finial
            v(0.07, 1.06),
            v(0.07, 1.12),
            v(0.13, 1.12),
            v(0.13, 1.17),
            v(0.07, 1.17),
            v(0.07, 1.24),
            v(0.00, 1.26),
        ],
        // Stylized, deliberately NOT a horse — a lathe can't make an asymmetric
        // knight. A faceted, stepped turned shape that still reads as its own
        // piece. See the README note.
        Kind::Knight => vec![
            v(0.00, 0.00),
            v(0.33, 0.00),
            v(0.33, 0.05),
            v(0.22, 0.11),
            v(0.20, 0.24),
            v(0.235, 0.34),
            v(0.16, 0.40),
            v(0.235, 0.46),
            v(0.16, 0.52),
            v(0.235, 0.58),
            v(0.16, 0.64),
            v(0.20, 0.70),
            v(0.115, 0.76),
            v(0.075, 0.82),
            v(0.00, 0.84),
        ],
    }
}
