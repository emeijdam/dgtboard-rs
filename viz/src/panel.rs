//! An on-screen control panel: what the web page's HTML panel does, as Bevy
//! UI for the native window — view buttons, the session's commands (resign,
//! abort, next puzzle, quit) and a legend of the keys. Clicking does exactly
//! what the key does.

use bevy::prelude::*;

use super::overlay::{Overlay, View, FONT};
#[cfg(not(target_arch = "wasm32"))]
use super::session::{Command, SessionRes};

/// Which buttons the panel needs, decided at launch.
#[derive(Resource, Clone, Copy, Debug, Default)]
pub struct PanelConfig {
    /// A Lichess game or puzzle session runs (resign / abort / next / quit).
    pub session: bool,
    /// The session is puzzles ("Next puzzle" rather than resign / abort).
    pub puzzle: bool,
}

/// What a button does.
#[derive(Component, Clone, Copy, Debug)]
pub(super) enum Action {
    ToggleView,
    ToggleSide,
    Resign,
    Abort,
    Next,
    Quit,
}

/// A button's resting colours, to restore after hover / press.
#[derive(Component, Clone, Copy)]
pub(super) struct Resting {
    background: Color,
    hover: Color,
}

const INK: Color = Color::srgb(0.92, 0.93, 0.97);
const MUTED: Color = Color::srgb(0.62, 0.65, 0.76);
const ACCENT: Color = Color::srgb(0.43, 0.66, 1.0);
const ACCENT_HOVER: Color = Color::srgb(0.56, 0.74, 1.0);
const GHOST: Color = Color::srgba(1.0, 1.0, 1.0, 0.06);
const GHOST_HOVER: Color = Color::srgba(1.0, 1.0, 1.0, 0.14);
const DANGER: Color = Color::srgba(0.95, 0.35, 0.35, 0.18);
const DANGER_HOVER: Color = Color::srgba(0.95, 0.35, 0.35, 0.34);

/// Build the panel at the right edge, under the clocks.
pub fn setup(mut commands: Commands, asset_server: Res<AssetServer>, config: Res<PanelConfig>) {
    let font = asset_server.load::<Font>(FONT);
    let text = |size: f32, color: Color| TextStyle {
        font: font.clone(),
        font_size: size,
        color,
    };

    commands
        .spawn(NodeBundle {
            style: Style {
                position_type: PositionType::Absolute,
                right: Val::Px(18.0),
                top: Val::Px(64.0),
                width: Val::Px(228.0),
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(8.0),
                padding: UiRect::all(Val::Px(14.0)),
                ..default()
            },
            background_color: Color::srgba(0.06, 0.075, 0.13, 0.72).into(),
            border_radius: BorderRadius::all(Val::Px(14.0)),
            ..default()
        })
        .with_children(|panel| {
            panel.spawn(TextBundle::from_section("dgt3d", text(17.0, INK)));
            button(panel, "2D / 3D", Action::ToggleView, ACCENT, ACCENT_HOVER, Color::srgb(0.03, 0.06, 0.12), &text);
            button(panel, "View from Black", Action::ToggleSide, GHOST, GHOST_HOVER, INK, &text);
            if config.session {
                if config.puzzle {
                    button(panel, "Next puzzle", Action::Next, GHOST, GHOST_HOVER, INK, &text);
                } else {
                    button(panel, "Resign", Action::Resign, DANGER, DANGER_HOVER, INK, &text);
                    button(panel, "Abort", Action::Abort, GHOST, GHOST_HOVER, INK, &text);
                }
                button(panel, "Quit", Action::Quit, GHOST, GHOST_HOVER, INK, &text);
            }
            let legend = if config.session {
                if config.puzzle {
                    "Keys: Tab 2D/3D · F side · Enter next · Esc quit\nLeft-drag orbits, scroll zooms."
                } else {
                    "Keys: Tab 2D/3D · F side · R R resign · A abort · Esc quit (the game keeps running)\nLeft-drag orbits, scroll zooms."
                }
            } else {
                "Keys: Tab 2D/3D · F side\nLeft-drag orbits, scroll zooms. Free play is refereed from the start position."
            };
            panel.spawn(TextBundle::from_section(legend, text(12.0, MUTED)));
        });
}

#[allow(clippy::too_many_arguments)]
fn button(
    parent: &mut ChildBuilder,
    label: &str,
    action: Action,
    background: Color,
    hover: Color,
    ink: Color,
    text: &dyn Fn(f32, Color) -> TextStyle,
) {
    parent
        .spawn((
            ButtonBundle {
                style: Style {
                    padding: UiRect::axes(Val::Px(14.0), Val::Px(8.0)),
                    justify_content: JustifyContent::Center,
                    ..default()
                },
                background_color: background.into(),
                border_radius: BorderRadius::all(Val::Px(9.0)),
                ..default()
            },
            action,
            Resting { background, hover },
        ))
        .with_children(|b| {
            b.spawn(TextBundle::from_section(label, text(14.0, ink)));
        });
}

/// Hover and press feedback, and the actions themselves.
#[allow(clippy::type_complexity)]
pub fn interact(
    mut buttons: Query<(&Interaction, &Action, &Resting, &mut BackgroundColor), Changed<Interaction>>,
    mut view: ResMut<View>,
    #[cfg(not(target_arch = "wasm32"))] mut session: Option<ResMut<SessionRes>>,
    #[cfg(not(target_arch = "wasm32"))] mut overlay: ResMut<Overlay>,
    mut exit: EventWriter<AppExit>,
) {
    for (interaction, action, resting, mut background) in &mut buttons {
        match interaction {
            Interaction::Hovered => *background = resting.hover.into(),
            Interaction::None => *background = resting.background.into(),
            Interaction::Pressed => {
                *background = resting.hover.into();
                match action {
                    Action::ToggleView => view.two_d = !view.two_d,
                    Action::ToggleSide => view.from_black = !view.from_black,
                    Action::Quit => {
                        #[cfg(not(target_arch = "wasm32"))]
                        if let Some(s) = session.as_deref_mut() {
                            s.command(Command::Quit, &mut overlay);
                        }
                        exit.send(AppExit::Success);
                    }
                    Action::Resign | Action::Abort | Action::Next => {
                        #[cfg(not(target_arch = "wasm32"))]
                        if let Some(s) = session.as_deref_mut() {
                            let command = match action {
                                Action::Resign => Command::Resign,
                                Action::Abort => Command::Abort,
                                _ => Command::Next,
                            };
                            s.command(command, &mut overlay);
                        }
                    }
                }
            }
        }
    }
}
