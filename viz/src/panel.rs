//! An on-screen control panel: what the web page's HTML panel does, as Bevy
//! UI for the native window — view buttons, the session's commands (resign,
//! abort, next puzzle, quit) and a legend of the keys. Clicking does exactly
//! what the key does — and, like the keys, anything that ends or leaves a
//! game must be **held** for a moment: a bar fills on the button and in the
//! HUD, and letting go early does nothing. Small hands tap things.
//!
//! Every button carries a picture (see `assets/icons/`), so a child who
//! can't read yet can still use it; the two toggles show what they switch
//! *to* (the flat board while in 3D, the black king while viewing from
//! White).

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

/// The button pictures, loaded once.
#[derive(Resource)]
pub(super) struct Icons {
    board_flat: Handle<Image>,
    board_3d: Handle<Image>,
    king_black: Handle<Image>,
    king_white: Handle<Image>,
    puzzle: Handle<Image>,
    flag: Handle<Image>,
    abort: Handle<Image>,
    quit: Handle<Image>,
}

/// A button's picture (for the toggles to swap).
#[derive(Component)]
pub(super) struct Icon;

/// A button's label (for the toggles to swap).
#[derive(Component)]
pub(super) struct Label;

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
    let icon = |name: &str| asset_server.load::<Image>(format!("icons/{name}.png"));
    let icons = Icons {
        board_flat: icon("board_flat"),
        board_3d: icon("board_3d"),
        king_black: icon("king_black"),
        king_white: icon("king_white"),
        puzzle: icon("puzzle"),
        flag: icon("flag"),
        abort: icon("abort"),
        quit: icon("quit"),
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
            let b = |panel: &mut ChildBuilder, label, action, bg, hover, ink, img: &Handle<Image>| {
                button(panel, label, action, bg, hover, ink, &text, img.clone())
            };
            b(panel, "2D", Action::ToggleView, ACCENT, ACCENT_HOVER, Color::srgb(0.03, 0.06, 0.12), &icons.board_flat);
            b(panel, "View from Black", Action::ToggleSide, GHOST, GHOST_HOVER, INK, &icons.king_black);
            if config.session {
                if config.puzzle {
                    b(panel, "Next puzzle", Action::Next, GHOST, GHOST_HOVER, INK, &icons.puzzle);
                } else {
                    b(panel, "Resign", Action::Resign, DANGER, DANGER_HOVER, INK, &icons.flag);
                    b(panel, "Abort", Action::Abort, GHOST, GHOST_HOVER, INK, &icons.abort);
                }
                b(panel, "Quit", Action::Quit, GHOST, GHOST_HOVER, INK, &icons.quit);
            }
            let legend = if config.session {
                if config.puzzle {
                    "Hold a button for a moment to confirm.\nKeys: Tab · F · Enter · Esc (hold)"
                } else {
                    "Hold a button for a moment to confirm.\nKeys: Tab · F · R · A · Esc (hold)"
                }
            } else {
                "Keys: Tab 2D/3D · F side\nFree play, refereed from the start position."
            };
            panel.spawn(TextBundle::from_section(legend, text(12.0, MUTED)));
        });
    commands.insert_resource(icons);
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
    icon: Handle<Image>,
) {
    let mut button = parent.spawn((
        ButtonBundle {
            style: Style {
                padding: UiRect::axes(Val::Px(12.0), Val::Px(8.0)),
                align_items: AlignItems::Center,
                column_gap: Val::Px(10.0),
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
        b.spawn((
            ImageBundle {
                style: Style {
                    width: Val::Px(30.0),
                    height: Val::Px(30.0),
                    ..default()
                },
                image: UiImage::new(icon),
                ..default()
            },
            Icon,
        ));
        b.spawn((TextBundle::from_section(label, text(15.0, ink)), Label));
    });
}

/// Keep the two toggles showing what they switch *to*.
#[allow(clippy::type_complexity)]
pub fn refresh_toggles(
    view: Res<View>,
    icons: Option<Res<Icons>>,
    buttons: Query<(&Action, &Children)>,
    mut images: Query<&mut UiImage, With<Icon>>,
    mut labels: Query<&mut Text, With<Label>>,
) {
    let Some(icons) = icons else {
        return;
    };
    if !view.is_changed() {
        return;
    }
    for (action, children) in &buttons {
        let (icon, label) = match action {
            Action::ToggleView if view.two_d => (&icons.board_3d, "3D"),
            Action::ToggleView => (&icons.board_flat, "2D"),
            Action::ToggleSide if view.from_black => (&icons.king_white, "View from White"),
            Action::ToggleSide => (&icons.king_black, "View from Black"),
            _ => continue,
        };
        for child in children {
            if let Ok(mut image) = images.get_mut(*child) {
                image.texture = icon.clone();
            }
            if let Ok(mut text) = labels.get_mut(*child) {
                if let Some(section) = text.sections.first_mut() {
                    section.value = label.to_string();
                }
            }
        }
    }
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
