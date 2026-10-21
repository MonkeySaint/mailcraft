use serde::{Deserialize, Serialize};

use crate::{DateTime, FolderId, ItemId};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum NoteColor {
    Blue,
    Green,
    Pink,
    #[default]
    Yellow,
    White,
}

/// Largest icon offset kept (points from the Icon view grid's top-left corner).
pub const MAX_NOTE_OFFSET: f32 = 100_000.0;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Note {
    pub id: ItemId,
    pub folder: FolderId,
    /// First line is the title.
    pub text: String,
    pub color: NoteColor,
    pub created: DateTime,
    pub modified: DateTime,
    pub categories: Vec<String>,
    /// Icon view: where the icon was dragged to (offset from the grid's top-left corner, points). Both set
    /// means off the grid; `None` means in its grid place (Line Up Icons and Reset View clear them).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub x: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub y: Option<f32>,
}

/// Whether `v` is a usable icon offset (finite, 0 to [`MAX_NOTE_OFFSET`]).
pub fn valid_note_offset(v: f32) -> bool {
    v.is_finite() && (0.0..=MAX_NOTE_OFFSET).contains(&v)
}

impl Note {
    /// The icon's dragged position, when both offsets are set and valid; damaged values read as "on the
    /// grid".
    pub fn position(&self) -> Option<(f32, f32)> {
        match (self.x, self.y) {
            (Some(x), Some(y)) if valid_note_offset(x) && valid_note_offset(y) => Some((x, y)),
            _ => None,
        }
    }

    /// Puts the icon at `pos` (`None`: back on the grid). Offsets out of range are clamped, junk reads as 0.
    pub fn set_position(&mut self, pos: Option<(f32, f32)>) {
        let fit = |v: f32| if v.is_finite() { v.clamp(0.0, MAX_NOTE_OFFSET) } else { 0.0 };
        (self.x, self.y) = match pos {
            Some((x, y)) => (Some(fit(x)), Some(fit(y))),
            None => (None, None),
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positions_are_validated_and_clamped() {
        let mut n = Note::default();
        assert_eq!(n.position(), None);
        n.set_position(Some((12.5, 40.0)));
        assert_eq!(n.position(), Some((12.5, 40.0)));
        n.set_position(Some((f32::NAN, 1e30)));
        assert_eq!(n.position(), Some((0.0, MAX_NOTE_OFFSET)));
        n.set_position(None);
        assert_eq!((n.x, n.y), (None, None));
        // One offset alone, or damaged ones from a hand-edited file, mean "on the grid".
        n.x = Some(3.0);
        assert_eq!(n.position(), None);
        n.y = Some(-1.0);
        assert_eq!(n.position(), None);
        // Old files without the fields read as on the grid; empty fields are not written.
        let old: Note = serde_json::from_str(r#"{"id":1,"folder":2,"text":"a","color":"Blue","created":{"unix":0,"offset_min":0},"modified":{"unix":0,"offset_min":0},"categories":[]}"#).unwrap();
        assert_eq!(old.position(), None);
        assert!(!serde_json::to_string(&old).unwrap().contains("\"x\""));
    }
}
