//! The overlay: everything drawn *on top of* the live board — square marks
//! (last move, expected reply, check, the squares a resync needs touched), a
//! translucent "ghost" of the piece the opponent just moved, and a HUD with a
//! title, clocks, a status line and an optional banner.
//!
//! The [`Overlay`] resource is the contract: plain data, filled from outside
//! (a Lichess or puzzle session, a demo) and replaced wholesale whenever it
//! changes (`*overlay = new` — it's a few strings and a small map, so that's
//! cheap). Nothing in here knows where the data comes from. Code that runs
//! off the Bevy schedule (a reader thread, the JS bridge) hands a new
//! overlay to [`post_overlay`], which a system drains into the resource.
//!
//! This module also owns the 3D rendering of the overlay (thin translucent
//! slabs on the marked squares, the ghost piece, the `bevy_ui` HUD) and the
//! [`View`] resource — 2D or 3D, seen from White or Black — with its keys
//! (`Tab`, `F`). The 2D rendering of the same data lives in [`flat`](super::flat).

use std::collections::HashMap;
use std::sync::Mutex;

pub use bevy::utils::Instant;

use bevy::color::Alpha;
use bevy::prelude::*;
use dgtboard_core::Color as PieceColor;
use dgtboard_core::Piece;

use super::pieces::kind_index;
use super::{set, square_world, PieceAssets, PieceSet};

/// The HUD / label font, relative to the asset root: a DejaVu Sans subset
/// (see `assets/fonts/ATTRIBUTION.md`) because Bevy's built-in font has no
/// chess, arrow or dash glyphs — and session lines are full of them.
pub const FONT: &str = "fonts/DejaVuSans-subset.ttf";

/// What a marked square means. One mark per square; a later mark replaces an
/// earlier one.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Mark {
    /// Origin of the last move played (amber, like Lichess).
    LastFrom,
    /// Destination of the last move played.
    LastTo,
    /// Origin of the move we are waiting for the player to make on the board.
    ExpectFrom,
    /// Destination of that expected move.
    ExpectTo,
    /// The king in check.
    Check,
    /// Resync: a piece has to be put on this square.
    Place,
    /// Resync: the piece on this square has to be taken off.
    Remove,
    /// Resync: the piece on this square is the wrong one.
    Swap,
}

impl Mark {
    /// Every mark, for building per-mark assets.
    pub const ALL: [Mark; 8] = [
        Mark::LastFrom,
        Mark::LastTo,
        Mark::ExpectFrom,
        Mark::ExpectTo,
        Mark::Check,
        Mark::Place,
        Mark::Remove,
        Mark::Swap,
    ];

    /// The mark's translucent colour, shared by the 2D and 3D views. Last
    /// move is Lichess's amber; check and "remove" are both red, with check
    /// the deeper crimson so the two never get confused side by side.
    pub fn color(self) -> Color {
        match self {
            Mark::LastFrom | Mark::LastTo => Color::srgba_u8(155, 199, 0, 105),
            Mark::ExpectFrom | Mark::ExpectTo => Color::srgba_u8(40, 110, 240, 120),
            Mark::Check => Color::srgba_u8(200, 20, 30, 150),
            Mark::Place => Color::srgba_u8(40, 200, 80, 130),
            Mark::Remove => Color::srgba_u8(240, 70, 60, 130),
            Mark::Swap => Color::srgba_u8(255, 150, 0, 130),
        }
    }
}

/// Everything drawn over the board. Replace it wholesale; the render systems
/// diff against what they last drew.
#[derive(Resource, Default, Clone, Debug)]
pub struct Overlay {
    /// Marked squares, keyed by DGT index (0 = a8 … 63 = h1).
    pub marks: HashMap<usize, Mark>,
    /// The piece the opponent moved, drawn translucent on its destination
    /// square until the player copies the move on the physical board.
    pub ghost: Option<(usize, Piece)>,
    /// The text layer.
    pub hud: Hud,
}

/// The HUD's text. Empty strings and `None`s draw nothing.
#[derive(Default, Clone, Debug)]
pub struct Hud {
    /// Top-left: what we're playing ("Stockfish level 1 · 15+10").
    pub title: String,
    /// Top-right: both clocks, the ticking one counting down locally.
    pub clocks: Option<Clocks>,
    /// Bottom: the one-line status ("← Black plays Nf6 — play it on the board …").
    pub line: String,
    /// Centred, large: a result or a warning. `None` hides it.
    pub banner: Option<String>,
}

/// Both clocks as last reported, plus when that was, so the ticking side can
/// be counted down locally between reports.
#[derive(Clone, Copy, Debug)]
pub struct Clocks {
    /// White's remaining time at `since`.
    pub white_ms: u64,
    /// Black's remaining time at `since`.
    pub black_ms: u64,
    /// Whose clock is running, if either.
    pub ticking: Option<PieceColor>,
    /// When the two values were sampled. [`Instant`] is Bevy's, which also
    /// works in the browser (std's panics on wasm).
    pub since: Instant,
}

impl Clocks {
    /// The clocks as they read `now`: the ticking side's value less the time
    /// elapsed since the sample, floored at zero.
    pub fn remaining(&self, now: Instant) -> (u64, u64) {
        let elapsed = now.saturating_duration_since(self.since).as_millis() as u64;
        match self.ticking {
            Some(PieceColor::White) => (self.white_ms.saturating_sub(elapsed), self.black_ms),
            Some(PieceColor::Black) => (self.white_ms, self.black_ms.saturating_sub(elapsed)),
            None => (self.white_ms, self.black_ms),
        }
    }
}

/// Format a clock like Lichess: `h:mm:ss` over an hour, `mm:ss` otherwise,
/// tenths once under ten seconds.
pub fn clock_text(ms: u64) -> String {
    let total = ms / 1000;
    let (h, m, s) = (total / 3600, (total / 60) % 60, total % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else if ms < 10_000 {
        format!("{m:02}:{s:02}.{}", (ms / 100) % 10)
    } else {
        format!("{m:02}:{s:02}")
    }
}

/// Which view is on screen: the flat Lichess-style board or the 3D scene, and
/// whether it is seen from Black's side.
#[derive(Resource, Default, Clone, Copy, PartialEq, Eq, Debug)]
pub struct View {
    /// Show the 2D board instead of the 3D scene.
    pub two_d: bool,
    /// Look at the board from Black's side (rank 8 nearest / at the bottom).
    pub from_black: bool,
}

/// DGT index of a square from a UCI-style name (`"e4"`), `None` if malformed.
pub fn square(name: &str) -> Option<usize> {
    let mut chars = name.chars();
    let file = chars.next()?.to_ascii_lowercase();
    let rank = chars.next()?.to_digit(10)?;
    if chars.next().is_some() || !('a'..='h').contains(&file) || !(1..=8).contains(&rank) {
        return None;
    }
    Some((8 - rank as usize) * 8 + (file as usize - 'a' as usize))
}

/// Overlays posted from outside the Bevy schedule, applied by
/// [`apply_posted`]. Only the latest matters, so a slot rather than a queue.
static POSTED: Mutex<Option<Overlay>> = Mutex::new(None);

/// Replace the overlay from anywhere — another thread, the JS bridge. Takes
/// effect on the next frame.
pub fn post_overlay(overlay: Overlay) {
    *POSTED.lock().unwrap() = Some(overlay);
}

/// View changes requested from outside the schedule (`Some` = set), applied
/// by [`apply_posted`].
static POSTED_VIEW: Mutex<(Option<bool>, Option<bool>)> = Mutex::new((None, None));

/// Request a view change from anywhere: `two_d` and/or `from_black`, each
/// left alone when `None`. Takes effect on the next frame.
pub fn post_view(two_d: Option<bool>, from_black: Option<bool>) {
    let mut slot = POSTED_VIEW.lock().unwrap();
    if two_d.is_some() {
        slot.0 = two_d;
    }
    if from_black.is_some() {
        slot.1 = from_black;
    }
}

/// A canned mid-game overlay, for eyeballing the rendering without a game:
/// White has just played e2–e4, the engine answered Nf6 and we're waiting for
/// the player to copy it, a resync wants a piece placed on d4.
pub fn demo() -> Overlay {
    let mut marks = HashMap::new();
    marks.insert(square("e2").unwrap(), Mark::LastFrom);
    marks.insert(square("e4").unwrap(), Mark::LastTo);
    marks.insert(square("g8").unwrap(), Mark::ExpectFrom);
    marks.insert(square("f6").unwrap(), Mark::ExpectTo);
    marks.insert(square("d4").unwrap(), Mark::Place);
    Overlay {
        marks,
        ghost: Some((square("f6").unwrap(), Piece::BlackKnight)),
        hud: Hud {
            title: "Stockfish level 1 · 15+10".into(),
            clocks: Some(Clocks {
                white_ms: 14 * 60_000 + 52_000,
                black_ms: 15 * 60_000 + 7_000,
                ticking: Some(PieceColor::Black),
                since: Instant::now(),
            }),
            line: "← Black plays Nf6 — play it on the board …".into(),
            banner: None,
        },
    }
}

/// Drain [`POSTED`] and [`POSTED_VIEW`] into the resources. Written so the
/// resources are only marked changed when something was actually posted.
pub(super) fn apply_posted(mut overlay: ResMut<Overlay>, mut view: ResMut<View>) {
    if let Some(new) = POSTED.lock().unwrap().take() {
        *overlay = new;
    }
    let (two_d, from_black) = std::mem::take(&mut *POSTED_VIEW.lock().unwrap());
    if let Some(two_d) = two_d {
        view.two_d = two_d;
    }
    if let Some(from_black) = from_black {
        view.from_black = from_black;
    }
}

/// `Tab` toggles 2D/3D, `F` flips the side the board is seen from. (Game
/// commands don't belong here; they're the session's.)
pub(super) fn view_keys(keys: Res<ButtonInput<KeyCode>>, mut view: ResMut<View>) {
    if keys.just_pressed(KeyCode::Tab) {
        view.two_d = !view.two_d;
    }
    if keys.just_pressed(KeyCode::KeyF) {
        view.from_black = !view.from_black;
    }
}

/// Switch cameras when [`View::two_d`] changes: exactly one of the two is
/// active, and the HUD follows it (`bevy_ui` draws on the default UI camera,
/// which is a marker we move rather than the highest-order camera, since that
/// one may be the inactive one).
#[allow(clippy::type_complexity)]
pub(super) fn apply_view(
    mut commands: Commands,
    view: Res<View>,
    mut cams: Query<(Entity, &mut Camera, Has<Camera2d>), Or<(With<Camera2d>, With<Camera3d>)>>,
) {
    if !view.is_changed() {
        return;
    }
    for (entity, mut cam, flat) in &mut cams {
        let active = flat == view.two_d;
        cam.is_active = active;
        if active {
            commands.entity(entity).insert(IsDefaultUiCamera);
        } else {
            commands.entity(entity).remove::<IsDefaultUiCamera>();
        }
    }
}

// ---------------------------------------------------------------- 3D marks

/// One of the 64 translucent slabs that sit just above the board surface,
/// shown with the colour of the square's mark.
#[derive(Component)]
pub(super) struct Slab(usize);

/// One material per [`Mark`], indexed like [`Mark::ALL`].
#[derive(Resource)]
pub(super) struct SlabMaterials([Handle<StandardMaterial>; 8]);

/// Spawn the 64 hidden slabs and the per-mark materials.
pub(super) fn setup_3d(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    // Unlit so the mark reads as its exact colour whatever the lighting does,
    // blended so the square (and any piece base) shows through. The shared
    // colours are tuned for the flat board; over the lit, tonemapped marble
    // the light ones (amber) wash out, so here they're a little deeper and
    // more opaque.
    let mats = Mark::ALL.map(|m| {
        let c = Srgba::from(m.color());
        materials.add(StandardMaterial {
            base_color: Color::srgba(
                c.red * 0.8,
                c.green * 0.8,
                c.blue * 0.8,
                c.alpha.max(0.62),
            ),
            alpha_mode: AlphaMode::Blend,
            unlit: true,
            ..default()
        })
    });
    // Just above the board top (y = 0) so it never z-fights with the square.
    let slab = meshes.add(Cuboid::new(0.96, 0.02, 0.96));
    for idx in 0..64 {
        commands.spawn((
            PbrBundle {
                mesh: slab.clone(),
                material: mats[0].clone(),
                transform: Transform::from_translation(square_world(idx) + Vec3::Y * 0.011),
                visibility: Visibility::Hidden,
                ..default()
            },
            Slab(idx),
        ));
    }
    commands.insert_resource(SlabMaterials(mats));
}

/// Show each slab with its square's mark, hide the rest.
pub(super) fn sync_marks_3d(
    overlay: Res<Overlay>,
    mats: Res<SlabMaterials>,
    mut slabs: Query<(&Slab, &mut Visibility, &mut Handle<StandardMaterial>)>,
) {
    if !overlay.is_changed() {
        return;
    }
    for (slab, mut vis, mut mat) in &mut slabs {
        match overlay.marks.get(&slab.0) {
            Some(mark) => {
                let want = &mats.0[Mark::ALL.iter().position(|m| m == mark).unwrap()];
                if *mat != *want {
                    *mat = want.clone();
                }
                *vis = Visibility::Inherited;
            }
            None => *vis = Visibility::Hidden,
        }
    }
}

// ---------------------------------------------------------------- 3D ghost

/// The ghost piece as currently spawned in 3D: what it shows and whether it
/// came from the scanned set (so it's respawned when the set resolves).
#[derive(Resource, Default)]
pub(super) struct Ghost3d(Option<(usize, Piece, bool, Entity)>);

/// Keep the 3D ghost in step with [`Overlay::ghost`]: respawn when the
/// square/piece changes or the scanned set arrives, despawn when cleared.
pub(super) fn sync_ghost_3d(
    mut commands: Commands,
    overlay: Res<Overlay>,
    assets: Option<Res<PieceAssets>>,
    set: Option<Res<PieceSet>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut ghost: Local<Ghost3d>,
) {
    let Some(assets) = assets else {
        return;
    };
    let want = overlay.ghost.map(|(sq, p)| (sq, p, set.is_some()));
    let have = ghost.0.map(|(sq, p, scanned, _)| (sq, p, scanned));
    if want == have {
        return;
    }
    if let Some((_, _, _, e)) = ghost.0.take() {
        commands.entity(e).despawn_recursive();
    }
    let Some((sq, p, scanned)) = want else {
        return;
    };
    let black = p.color() == PieceColor::Black;
    let entity = match set.as_deref() {
        Some(set) => {
            let base = if black { &set.black } else { &set.white };
            let material = ghost_material(&mut materials, base);
            set::spawn_piece_with(&mut commands, set, kind_index(p), black, sq, material)
        }
        None => {
            let base = if black { &assets.black } else { &assets.white };
            let material = ghost_material(&mut materials, base);
            super::spawn_lathe_piece_with(&mut commands, &assets, p, black, sq, material)
        }
    };
    ghost.0 = Some((sq, p, scanned, entity));
}

/// A copy of `base` at 45 % alpha with a cool emissive glow, so the ghost
/// reads as "not really there" yet still catches the eye — the scanned black
/// marble at 45 % over a dark square is otherwise all but invisible.
fn ghost_material(
    materials: &mut Assets<StandardMaterial>,
    base: &Handle<StandardMaterial>,
) -> Handle<StandardMaterial> {
    let mut m = materials.get(base).cloned().unwrap_or_default();
    m.base_color = m.base_color.with_alpha(0.45);
    m.alpha_mode = AlphaMode::Blend;
    m.emissive += LinearRgba::rgb(0.28, 0.40, 0.75);
    materials.add(m)
}

// ---------------------------------------------------------------- HUD

#[derive(Component)]
pub(super) struct HudTitle;
#[derive(Component)]
pub(super) struct HudClocks;
#[derive(Component)]
pub(super) struct HudLine;
#[derive(Component)]
pub(super) struct HudBanner;
#[derive(Component)]
pub(super) struct HudBannerText;

/// Spawn the HUD nodes once; `sync_hud` fills them every frame.
pub(super) fn setup_hud(mut commands: Commands, asset_server: Res<AssetServer>) {
    let ink = Color::srgb(0.92, 0.93, 0.97);
    let muted = Color::srgb(0.62, 0.65, 0.76);
    let font = asset_server.load::<Font>(FONT);
    let text = |size: f32, color: Color| TextStyle {
        font: font.clone(),
        font_size: size,
        color,
    };
    let at = |left: Val, right: Val, top: Val, bottom: Val| Style {
        position_type: PositionType::Absolute,
        left,
        right,
        top,
        bottom,
        ..default()
    };

    commands.spawn((
        TextBundle::from_section("", text(22.0, ink))
            .with_style(at(Val::Px(18.0), Val::Auto, Val::Px(14.0), Val::Auto)),
        HudTitle,
    ));
    commands.spawn((
        TextBundle::from_sections([
            TextSection::new("", text(26.0, muted)),
            TextSection::new("", text(26.0, muted)),
        ])
        .with_style(at(Val::Auto, Val::Px(18.0), Val::Px(12.0), Val::Auto)),
        HudClocks,
    ));
    commands.spawn((
        TextBundle::from_section("", text(19.0, ink))
            .with_text_justify(JustifyText::Center)
            .with_style(Style {
                width: Val::Percent(100.0),
                ..at(Val::Px(0.0), Val::Auto, Val::Auto, Val::Px(16.0))
            }),
        HudLine,
    ));
    // The banner: a dark pill centred a little above the middle.
    commands
        .spawn((
            NodeBundle {
                style: Style {
                    width: Val::Percent(100.0),
                    justify_content: JustifyContent::Center,
                    ..at(Val::Px(0.0), Val::Auto, Val::Percent(38.0), Val::Auto)
                },
                visibility: Visibility::Hidden,
                ..default()
            },
            HudBanner,
        ))
        .with_children(|parent| {
            parent
                .spawn(NodeBundle {
                    style: Style {
                        padding: UiRect::axes(Val::Px(28.0), Val::Px(14.0)),
                        ..default()
                    },
                    background_color: Color::srgba(0.02, 0.02, 0.05, 0.72).into(),
                    border_radius: BorderRadius::all(Val::Px(16.0)),
                    ..default()
                })
                .with_children(|pill| {
                    pill.spawn((
                        TextBundle::from_section("", text(40.0, ink)),
                        HudBannerText,
                    ));
                });
        });
}

/// Copy [`Hud`] into the nodes; the clocks tick, so this runs every frame.
#[allow(clippy::type_complexity)]
pub(super) fn sync_hud(
    overlay: Res<Overlay>,
    mut texts: ParamSet<(
        Query<&mut Text, With<HudTitle>>,
        Query<&mut Text, With<HudClocks>>,
        Query<&mut Text, With<HudLine>>,
        Query<&mut Text, With<HudBannerText>>,
    )>,
    mut banner: Query<&mut Visibility, With<HudBanner>>,
) {
    let hud = &overlay.hud;
    if overlay.is_changed() {
        set_text(&mut texts.p0(), &hud.title);
        set_text(&mut texts.p2(), &hud.line);
        set_text(&mut texts.p3(), hud.banner.as_deref().unwrap_or(""));
        for mut vis in &mut banner {
            *vis = if hud.banner.is_some() {
                Visibility::Inherited
            } else {
                Visibility::Hidden
            };
        }
    }

    // Clocks: ticking side bright and a size up; the other dimmed.
    let bright = Color::srgb(1.0, 1.0, 1.0);
    let dim = Color::srgb(0.55, 0.58, 0.68);
    for mut text in &mut texts.p1() {
        match hud.clocks {
            Some(clocks) => {
                let (w, b) = clocks.remaining(Instant::now());
                let style = |side| {
                    if clocks.ticking == Some(side) {
                        (bright, 30.0)
                    } else {
                        (dim, 26.0)
                    }
                };
                let (wc, ws) = style(PieceColor::White);
                let (bc, bs) = style(PieceColor::Black);
                text.sections[0].value = format!("♔ {}   ", clock_text(w));
                text.sections[0].style.color = wc;
                text.sections[0].style.font_size = ws;
                text.sections[1].value = format!("♚ {}", clock_text(b));
                text.sections[1].style.color = bc;
                text.sections[1].style.font_size = bs;
            }
            None => {
                text.sections[0].value.clear();
                text.sections[1].value.clear();
            }
        }
    }
}

fn set_text(query: &mut Query<&mut Text, impl bevy::ecs::query::QueryFilter>, value: &str) {
    for mut text in query.iter_mut() {
        if let Some(section) = text.sections.first_mut() {
            section.value = value.to_string();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn square_names() {
        assert_eq!(square("a8"), Some(0));
        assert_eq!(square("h1"), Some(63));
        assert_eq!(square("e2"), Some(52));
        assert_eq!(square("E4"), Some(36));
        assert_eq!(square("i1"), None);
        assert_eq!(square("e9"), None);
        assert_eq!(square("e"), None);
        assert_eq!(square("e44"), None);
    }

    #[test]
    fn clock_formats() {
        assert_eq!(clock_text(15 * 60_000 + 7_000), "15:07");
        assert_eq!(clock_text(3_600_000 + 61_000), "1:01:01");
        assert_eq!(clock_text(9_900), "00:09.9");
        assert_eq!(clock_text(0), "00:00.0");
    }

    #[test]
    fn clocks_count_the_ticking_side_down() {
        let since = Instant::now();
        let c = Clocks {
            white_ms: 10_000,
            black_ms: 20_000,
            ticking: Some(PieceColor::Black),
            since,
        };
        let later = since + std::time::Duration::from_millis(1_500);
        assert_eq!(c.remaining(later), (10_000, 18_500));
        let far = since + std::time::Duration::from_secs(60);
        assert_eq!(c.remaining(far), (10_000, 0));
    }
}
