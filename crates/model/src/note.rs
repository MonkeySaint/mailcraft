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

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Note {
    pub id: ItemId,
    pub folder: FolderId,
    /// First line is the title.
    pub text: String,
    pub color: NoteColor,
    pub created: DateTime,
    pub modified: DateTime,
    pub categories: Vec<String>,
}
