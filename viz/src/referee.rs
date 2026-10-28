//! Free play: a local referee that fills the [`Overlay`] when no Lichess or
//! puzzle session does — natively and in the browser.
//!
//! It referees from the standard start position with the core's
//! [`RefereedGame`]: each settled move on the board becomes the last-move
//! marks and a line like `3… Nf6`, check tints the king, mate and stalemate
//! raise the banner, an illegal move is explained, and if the board doesn't
//! show the start position the squares to fix are marked. Nothing is sent
//! anywhere; it's the `dgt referee` command, drawn.

use bevy::prelude::*;
use dgtboard_core::{Board, IllegalReason, RefereedGame, Ruling, Status};

use super::overlay::{square, Hud, Mark, Overlay};
use super::BoardRes;

/// The free-play referee's state.
#[derive(Resource)]
pub struct Referee {
    game: RefereedGame,
    /// The last board we refereed, to act only on changes.
    seen: Option<Board>,
    last_move: Option<(usize, usize)>,
    /// Half-moves played.
    ply: usize,
    line: String,
    banner: Option<String>,
}

impl Default for Referee {
    fn default() -> Self {
        Referee {
            game: RefereedGame::new(),
            seen: None,
            last_move: None,
            ply: 0,
            line: "Free play — moves are refereed from the start position.".into(),
            banner: None,
        }
    }
}

fn reason_text(reason: IllegalReason) -> &'static str {
    match reason {
        IllegalReason::NotYourTurn => "it's not your turn",
        IllegalReason::NoPieceThere => "there was no piece on that square",
        IllegalReason::MustGetOutOfCheck => "your king is in check",
        IllegalReason::OwnPieceOnTarget => "your own piece is already there",
        IllegalReason::IllegalMove => "that piece can't move like that",
    }
}

/// `e2e4` → the DGT indices of its two squares.
fn squares(uci: &str) -> Option<(usize, usize)> {
    Some((square(uci.get(0..2)?)?, square(uci.get(2..4)?)?))
}

/// Referee each change of the decoded board and redraw the overlay.
pub fn system(board: Res<BoardRes>, mut referee: ResMut<Referee>, mut overlay: ResMut<Overlay>) {
    let current = board.decoder.board();
    if referee.seen.as_ref() == Some(current) {
        return;
    }
    referee.seen = Some(current.clone());

    let mover = referee.game.turn();
    match referee.game.update(current) {
        Some(Ruling::Legal { uci, san, status }) => {
            referee.ply += 1;
            referee.last_move = squares(&uci);
            let number = (referee.ply + 1) / 2;
            let sep = if mover == dgtboard_core::Color::White { "." } else { "…" };
            referee.line = format!("{number}{sep} {san}");
            referee.banner = match status {
                Status::Normal => None,
                Status::Check => {
                    referee.line.push_str("  +");
                    None
                }
                Status::Checkmate { winner } => Some(format!("Checkmate — {winner} wins")),
                Status::Stalemate => Some("Stalemate — draw".into()),
                Status::DrawInsufficientMaterial => Some("Draw — insufficient material".into()),
            };
        }
        Some(Ruling::Illegal { uci, reason, piece }) => {
            referee.line = format!(
                "⚠ {uci} is not legal — {}. Put the {} back.",
                reason_text(reason),
                piece.glyph()
            );
        }
        Some(Ruling::BackInSync) => {
            referee.line = if referee.ply == 0 {
                format!("Start position set — {} to move.", referee.game.turn())
            } else {
                "✓ Back in position.".into()
            };
        }
        Some(Ruling::ExpectedPlayed { .. }) | Some(Ruling::WrongMove { .. }) | None => {}
    }

    let mut marks = std::collections::HashMap::new();
    if let Some((from, to)) = referee.last_move {
        marks.insert(from, Mark::LastFrom);
        marks.insert(to, Mark::LastTo);
    }
    if let Some(sq) = referee.game.checked_square() {
        marks.insert(sq, Mark::Check);
    }
    if !referee.game.in_sync() {
        let diff = referee.game.diff(current);
        for (sq, want, have) in &diff {
            let mark = match (want, have) {
                (Some(_), None) => Mark::Place,
                (None, Some(_)) => Mark::Remove,
                _ => Mark::Swap,
            };
            marks.insert(sq.0 as usize, mark);
        }
        if referee.ply == 0 && !diff.is_empty() {
            referee.line = format!(
                "Set up the start position to referee — {} square{} to fix",
                diff.len(),
                if diff.len() == 1 { "" } else { "s" }
            );
        }
    }
    *overlay = Overlay {
        marks,
        ghost: None,
        hud: Hud {
            title: format!("Free play · {} to move", referee.game.turn()),
            clocks: None,
            line: referee.line.clone(),
            banner: referee.banner.clone(),
        },
    };
}
