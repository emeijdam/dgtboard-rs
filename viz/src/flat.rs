//! The flat, Lichess-style board: the same live position and the same
//! [`Overlay`] as the 3D scene, drawn as 2D sprites for when you want to read
//! the position rather than admire it.
//!
//! Everything here lives on render layer 1, which only the 2D camera sees;
//! [`apply_view`](super::overlay::apply_view) activates that camera in place
//! of the 3D one. Sixty-four brown squares, file and rank labels, chessnut piece
//! sprites (Apache-2.0; see `assets/pieces/chessnut/ATTRIBUTION.md`) that
//! simply don't draw until their PNGs have loaded, translucent mark squares
//! and a 45 %-alpha ghost — all sized to the window each frame, mirrored when
//! the view is from Black.
//!
//! Pieces are reconciled from the same [`BoardRes`] the 3D `sync_system`
//! diffs, by a sibling system with its own entity map, so neither view knows
//! about the other.

use bevy::prelude::*;
use bevy::render::view::RenderLayers;
use bevy::window::PrimaryWindow;
use dgtboard_core::Color as PieceColor;
use dgtboard_core::Piece;

use super::overlay::{Overlay, View, FONT};
use super::pieces::kind_index;
use super::BoardRes;

/// Lichess's default "brown" board.
const LIGHT: Color = Color::srgb(0.941, 0.851, 0.710); // #f0d9b5
const DARK: Color = Color::srgb(0.710, 0.533, 0.388); // #b58863
const LABEL: Color = Color::srgb(0.70, 0.72, 0.80);

/// The layer the flat board lives on. The 2D camera also keeps the default
/// layer 0 so it sees the HUD (`bevy_ui` nodes are visibility-checked
/// against the active camera's layers); the 3D meshes there have no 2D
/// render phase, so they never show through.
const LAYER: usize = 1;

/// Draw order (z): squares, marks, labels, pieces, ghost.
const Z_SQUARE: f32 = 0.0;
const Z_MARK: f32 = 1.0;
const Z_LABEL: f32 = 1.5;
const Z_PIECE: f32 = 2.0;
const Z_GHOST: f32 = 3.0;

/// Something positioned by square: a square sprite, a mark, a piece or the
/// ghost. The layout system moves and sizes it.
#[derive(Component)]
pub(super) struct OnSquare {
    idx: usize,
    z: f32,
}

/// A file (`a`…`h`, 0-based) or rank (`1`…`8`, 0-based) label in the margin.
#[derive(Component)]
pub(super) enum Label {
    File(usize),
    Rank(usize),
}

/// The mark sprite of a square, hidden unless marked.
#[derive(Component)]
pub(super) struct MarkSprite;

/// The ghost piece sprite.
#[derive(Component)]
pub(super) struct GhostSprite;

/// The 2D camera.
#[derive(Component)]
pub(super) struct FlatCamera;

/// The twelve chessnut textures, indexed by [`texture_index`].
#[derive(Resource)]
pub(super) struct PieceTextures([Handle<Image>; 12]);

/// Index into [`PieceTextures`]: White's six kinds, then Black's.
fn texture_index(p: Piece) -> usize {
    let colour = if p.color() == PieceColor::Black { 6 } else { 0 };
    colour + kind_index(p)
}

/// The 2D pieces as spawned, diffed against the decoded board.
#[derive(Resource)]
pub(super) struct FlatBoard {
    pieces: [Option<(Entity, Piece)>; 64],
    ghost: Option<(usize, Piece)>,
}

impl Default for FlatBoard {
    fn default() -> Self {
        FlatBoard {
            pieces: [None; 64],
            ghost: None,
        }
    }
}

/// The board's current size on screen: the side of one square in logical
/// pixels, how far above the window centre the board sits (to leave the
/// HUD's bottom line room under the file labels), and the view it was laid
/// out for.
#[derive(Resource, Default, PartialEq, Clone, Copy)]
pub(super) struct Layout {
    square: f32,
    lift: f32,
    from_black: bool,
}

impl Layout {
    /// Centre of `idx` in world units (the 2D camera maps one unit to one
    /// logical pixel, origin at the window centre). From White, a8 is top
    /// left; from Black the board is rotated a half turn.
    fn centre(&self, idx: usize) -> Vec2 {
        let file = (idx % 8) as f32;
        let row = (idx / 8) as f32;
        self.place(Vec2::new(file - 3.5, 3.5 - row) * self.square)
    }

    /// Where a margin label goes: files under the bottom row, ranks left of
    /// the left column.
    fn label(&self, label: &Label) -> Vec2 {
        let edge = 4.0 * self.square + 0.42 * self.square;
        self.place(match *label {
            Label::File(f) => Vec2::new((f as f32 - 3.5) * self.square, -edge),
            Label::Rank(r) => Vec2::new(-edge, (r as f32 - 3.5) * self.square),
        })
    }

    /// A board-relative position (from White) on screen: mirrored through
    /// the centre when viewing from Black, then lifted.
    fn place(&self, v: Vec2) -> Vec2 {
        let v = if self.from_black { -v } else { v };
        v + Vec2::new(0.0, self.lift)
    }
}

/// Spawn the camera (inactive; `apply_view` switches), the squares, the mark
/// sprites and the labels, and start loading the piece textures.
pub(super) fn setup(mut commands: Commands, asset_server: Res<AssetServer>) {
    commands.spawn((
        Camera2dBundle {
            camera: Camera {
                // Above the 3D camera so there's never an order tie; only one
                // of the two is active at a time anyway.
                order: 1,
                is_active: false,
                ..default()
            },
            ..default()
        },
        RenderLayers::from_layers(&[0, LAYER]),
        FlatCamera,
    ));

    for idx in 0..64usize {
        let file = idx % 8;
        let rank = 8 - idx / 8;
        let light = (file + rank) % 2 == 0;
        commands.spawn((
            SpriteBundle {
                sprite: Sprite {
                    color: if light { LIGHT } else { DARK },
                    ..default()
                },
                ..default()
            },
            OnSquare { idx, z: Z_SQUARE },
            RenderLayers::layer(LAYER),
        ));
        commands.spawn((
            SpriteBundle {
                visibility: Visibility::Hidden,
                ..default()
            },
            OnSquare { idx, z: Z_MARK },
            MarkSprite,
            RenderLayers::layer(LAYER),
        ));
    }

    let style = TextStyle {
        font: asset_server.load(FONT),
        font_size: 16.0,
        color: LABEL,
    };
    for i in 0..8usize {
        let file = (b'a' + i as u8) as char;
        commands.spawn((
            Text2dBundle {
                text: Text::from_section(file.to_string(), style.clone()),
                ..default()
            },
            Label::File(i),
            RenderLayers::layer(LAYER),
        ));
        commands.spawn((
            Text2dBundle {
                text: Text::from_section((i + 1).to_string(), style.clone()),
                ..default()
            },
            Label::Rank(i),
            RenderLayers::layer(LAYER),
        ));
    }

    // White's six kinds in `pieces::KINDS` order, then Black's.
    let names = [
        "wP", "wN", "wB", "wR", "wQ", "wK", "bP", "bN", "bB", "bR", "bQ", "bK",
    ];
    let textures = names.map(|n| asset_server.load::<Image>(format!("pieces/chessnut/{n}.png")));
    commands.insert_resource(PieceTextures(textures));
    commands.init_resource::<FlatBoard>();
    commands.init_resource::<Layout>();
}

/// Fit the board to the window (with a margin for the labels) and, when the
/// size or the viewing side changed, move and resize everything.
pub(super) fn layout(
    windows: Query<&Window, With<PrimaryWindow>>,
    view: Res<View>,
    mut current: ResMut<Layout>,
    mut on_square: Query<(&OnSquare, &mut Transform, &mut Sprite)>,
    mut labels: Query<(&Label, &mut Transform, &mut Text), Without<OnSquare>>,
) {
    let Ok(window) = windows.get_single() else {
        return;
    };
    let side = window.width().min(window.height());
    // 10 % each side for the labels and the HUD, never a sub-pixel square,
    // and a touch above centre so the file labels clear the HUD's bottom line.
    let square = (side * 0.80 / 8.0).floor().max(8.0);
    let want = Layout {
        square,
        lift: (side * 0.015).round(),
        from_black: view.from_black,
    };
    if want == *current {
        return;
    }
    *current = want;

    let size = Some(Vec2::splat(square));
    for (sq, mut transform, mut sprite) in &mut on_square {
        transform.translation = want.centre(sq.idx).extend(sq.z);
        sprite.custom_size = size;
    }
    for (label, mut transform, mut text) in &mut labels {
        transform.translation = want.label(label).extend(Z_LABEL);
        for section in &mut text.sections {
            section.style.font_size = (square * 0.26).clamp(10.0, 28.0);
        }
    }
}

/// Spawn a piece sprite at `idx`, laid out for the current [`Layout`].
fn spawn_sprite(
    commands: &mut Commands,
    textures: &PieceTextures,
    layout: &Layout,
    p: Piece,
    idx: usize,
    z: f32,
    alpha: f32,
) -> Entity {
    commands
        .spawn((
            SpriteBundle {
                sprite: Sprite {
                    custom_size: Some(Vec2::splat(layout.square)),
                    color: Color::srgba(1.0, 1.0, 1.0, alpha),
                    ..default()
                },
                texture: textures.0[texture_index(p)].clone(),
                transform: Transform::from_translation(layout.centre(idx).extend(z)),
                ..default()
            },
            OnSquare { idx, z },
            RenderLayers::layer(LAYER),
        ))
        .id()
}

/// Reconcile the piece sprites with the decoded board, square by square.
pub(super) fn sync_pieces(
    mut commands: Commands,
    board: Res<BoardRes>,
    textures: Res<PieceTextures>,
    layout: Res<Layout>,
    mut flat: ResMut<FlatBoard>,
) {
    let squares = board.decoder.board().squares;
    for (idx, &want) in squares.iter().enumerate() {
        let have = flat.pieces[idx].map(|(_, p)| p);
        if want == have {
            continue;
        }
        if let Some((e, _)) = flat.pieces[idx].take() {
            commands.entity(e).despawn();
        }
        if let Some(p) = want {
            let e = spawn_sprite(&mut commands, &textures, &layout, p, idx, Z_PIECE, 1.0);
            flat.pieces[idx] = Some((e, p));
        }
    }
}

/// Colour and show the mark sprite of every marked square, hide the others,
/// and keep the ghost sprite in step with [`Overlay::ghost`].
pub(super) fn sync_overlay(
    mut commands: Commands,
    overlay: Res<Overlay>,
    textures: Res<PieceTextures>,
    layout: Res<Layout>,
    mut flat: ResMut<FlatBoard>,
    mut marks: Query<(&OnSquare, &mut Sprite, &mut Visibility), With<MarkSprite>>,
    ghosts: Query<Entity, With<GhostSprite>>,
) {
    if !overlay.is_changed() {
        return;
    }
    for (sq, mut sprite, mut vis) in &mut marks {
        match overlay.marks.get(&sq.idx) {
            Some(mark) => {
                sprite.color = mark.color();
                *vis = Visibility::Inherited;
            }
            None => *vis = Visibility::Hidden,
        }
    }

    if flat.ghost != overlay.ghost {
        for e in &ghosts {
            commands.entity(e).despawn();
        }
        if let Some((idx, p)) = overlay.ghost {
            let e = spawn_sprite(&mut commands, &textures, &layout, p, idx, Z_GHOST, 0.45);
            commands.entity(e).insert(GhostSprite);
        }
        flat.ghost = overlay.ghost;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_mirrors_from_black() {
        let white = Layout {
            square: 10.0,
            lift: 0.0,
            from_black: false,
        };
        let black = Layout {
            square: 10.0,
            lift: 0.0,
            from_black: true,
        };
        // a8 top-left from White, bottom-right from Black.
        assert_eq!(white.centre(0), Vec2::new(-35.0, 35.0));
        assert_eq!(black.centre(0), Vec2::new(35.0, -35.0));
        // h1 the other way round.
        assert_eq!(white.centre(63), Vec2::new(35.0, -35.0));
        assert_eq!(black.centre(63), Vec2::new(-35.0, 35.0));
        // The `a` label sits under the a-file, rank 1 left of the first row.
        assert_eq!(white.label(&Label::File(0)).x, -35.0);
        assert!(white.label(&Label::File(0)).y < -40.0);
        assert_eq!(white.label(&Label::Rank(0)).y, -35.0);
        assert_eq!(black.label(&Label::Rank(0)).y, 35.0);
        // The lift moves everything up, both ways round.
        let lifted = Layout { lift: 4.0, ..white };
        assert_eq!(lifted.centre(0), Vec2::new(-35.0, 39.0));
        let lifted = Layout { lift: 4.0, ..black };
        assert_eq!(lifted.centre(0), Vec2::new(35.0, -31.0));
    }

    #[test]
    fn textures_follow_kind_order() {
        assert_eq!(texture_index(Piece::WhitePawn), 0);
        assert_eq!(texture_index(Piece::WhiteKing), 5);
        assert_eq!(texture_index(Piece::BlackPawn), 6);
        assert_eq!(texture_index(Piece::BlackKing), 11);
    }
}
