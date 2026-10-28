//! An on-screen control panel: what the web page's HTML panel does, as Bevy
//! UI for the native window — view buttons, the session's commands (resign,
//! abort, next puzzle, quit) and a legend of the keys. Clicking does exactly
//! what the key does — and, like the keys, anything that ends or leaves a
//! game must be **held** for a moment: a bar fills on the button and in the
//! HUD, and letting go early does nothing. Small hands tap things.

use bevy::prelude::*;

use super::overlay::{Overlay, View, FONT};
#[cfg(not(target_arch = "wasm32"))]
use super::session::{hold_bar, Command, Holding, SessionRes};

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

/// A destructive button: its label, and the hold in progress if pressed.
#[derive(Component)]
pub(super) struct Hold {
    label: &'static str,
    #[cfg(not(target_arch = "wasm32"))]
    holding: Option<Holding>,
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
                    "Keys: Tab 2D/3D · F side · Enter next · hold Esc to quit\nHold a red/grey action for a moment to confirm it.\nLeft-drag orbits, scroll zooms."
                } else {
                    "Keys: Tab 2D/3D · F side · hold R to resign · hold A to abort · hold Esc to quit (the game keeps running)\nHold an action for a moment to confirm it.\nLeft-drag orbits, scroll zooms."
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
    let mut button = parent.spawn((
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
    ));
    if matches!(action, Action::Resign | Action::Abort | Action::Quit) {
        // Leak-free: labels are string literals in `setup`.
        let label: &'static str = match action {
            Action::Resign => "Resign",
            Action::Abort => "Abort",
            _ => "Quit",
        };
        button.insert(Hold {
            label,
            #[cfg(not(target_arch = "wasm32"))]
            holding: None,
        });
    }
    button.with_children(|b| {
        b.spawn(TextBundle::from_section(label, text(14.0, ink)));
    });
}

/// Hover and press feedback, and the instant actions (view, next puzzle).
/// Destructive buttons are handled by [`hold`].
#[allow(clippy::type_complexity)]
pub fn interact(
    mut buttons: Query<(&Interaction, &Action, &Resting, &mut BackgroundColor, Has<Hold>), Changed<Interaction>>,
    mut view: ResMut<View>,
    #[cfg(not(target_arch = "wasm32"))] mut session: Option<ResMut<SessionRes>>,
) {
    for (interaction, action, resting, mut background, held) in &mut buttons {
        match interaction {
            Interaction::Hovered => *background = resting.hover.into(),
            Interaction::None => *background = resting.background.into(),
            Interaction::Pressed => {
                *background = resting.hover.into();
                if held {
                    continue;
                }
                match action {
                    Action::ToggleView => view.two_d = !view.two_d,
                    Action::ToggleSide => view.from_black = !view.from_black,
                    Action::Next => {
                        #[cfg(not(target_arch = "wasm32"))]
                        if let Some(s) = session.as_deref_mut() {
                            s.command(Command::Next);
                        }
                    }
                    Action::Resign | Action::Abort | Action::Quit => {}
                }
            }
        }
    }
}

/// Hold-to-confirm for the destructive buttons: while pressed, the label
/// turns into a filling bar (mirrored in the HUD); at the end the command
/// runs; released early, the label comes back and nothing happens.
#[cfg(not(target_arch = "wasm32"))]
#[allow(clippy::type_complexity)]
pub fn hold(
    mut buttons: Query<(&Interaction, &Action, &mut Hold, &Children)>,
    mut labels: Query<&mut Text>,
    mut session: Option<ResMut<SessionRes>>,
    mut overlay: ResMut<Overlay>,
    mut exit: EventWriter<AppExit>,
) {
    for (interaction, action, mut hold, children) in &mut buttons {
        let command = match action {
            Action::Resign => Command::Resign,
            Action::Abort => Command::Abort,
            Action::Quit => Command::Quit,
            _ => continue,
        };
        let pressed = matches!(interaction, Interaction::Pressed);
        let label: String = match (pressed, hold.holding.as_ref()) {
            (false, None) => continue,
            (true, None) => {
                let previous = session.as_deref().map(|s| s.line().to_string()).unwrap_or_default();
                let h = Holding::start(command, &previous);
                if let Some(s) = session.as_deref_mut() {
                    s.show_line(&h.line(), &mut overlay);
                }
                hold.holding = Some(h);
                format!("{} {}", command.hold_label(), hold_bar(0.0))
            }
            (true, Some(h)) if h.progress() >= 1.0 => {
                let h = hold.holding.take().unwrap();
                let quit = match session.as_deref_mut() {
                    Some(s) => {
                        s.show_line(&h.previous_line, &mut overlay);
                        s.command(command)
                    }
                    None => command == Command::Quit,
                };
                if quit {
                    exit.send(AppExit::Success);
                }
                hold.label.to_string()
            }
            (true, Some(h)) => {
                let line = h.line();
                if let Some(s) = session.as_deref_mut() {
                    s.show_line(&line, &mut overlay);
                }
                format!("{} {}", command.hold_label(), hold_bar(h.progress()))
            }
            (false, Some(_)) => {
                let h = hold.holding.take().unwrap();
                if let Some(s) = session.as_deref_mut() {
                    s.show_line(&h.previous_line, &mut overlay);
                }
                hold.label.to_string()
            }
        };
        for child in children {
            if let Ok(mut text) = labels.get_mut(*child) {
                if let Some(section) = text.sections.first_mut() {
                    section.value = label.clone();
                }
            }
        }
    }
}
