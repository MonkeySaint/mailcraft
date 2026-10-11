//! Colour categories (Color Categories dialog: name, colour, shortcut key).

use mailcraft_geom::Color;
use serde::{Deserialize, Serialize};

/// The 25 preset category colours plus None. Our own palette (not sampled from any other product).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CategoryColor {
    #[default]
    None,
    Red,
    Orange,
    Peach,
    Yellow,
    Green,
    Teal,
    Olive,
    Blue,
    Purple,
    Maroon,
    Steel,
    DarkSteel,
    Gray,
    DarkGray,
    Black,
    DarkRed,
    DarkOrange,
    DarkPeach,
    DarkYellow,
    DarkGreen,
    DarkTeal,
    DarkOlive,
    DarkBlue,
    DarkPurple,
    DarkMaroon,
}

impl CategoryColor {
    /// Every colour in picker order (None first).
    pub const ALL: [CategoryColor; 26] = [
        CategoryColor::None,
        CategoryColor::Red,
        CategoryColor::Orange,
        CategoryColor::Peach,
        CategoryColor::Yellow,
        CategoryColor::Green,
        CategoryColor::Teal,
        CategoryColor::Olive,
        CategoryColor::Blue,
        CategoryColor::Purple,
        CategoryColor::Maroon,
        CategoryColor::Steel,
        CategoryColor::DarkSteel,
        CategoryColor::Gray,
        CategoryColor::DarkGray,
        CategoryColor::Black,
        CategoryColor::DarkRed,
        CategoryColor::DarkOrange,
        CategoryColor::DarkPeach,
        CategoryColor::DarkYellow,
        CategoryColor::DarkGreen,
        CategoryColor::DarkTeal,
        CategoryColor::DarkOlive,
        CategoryColor::DarkBlue,
        CategoryColor::DarkPurple,
        CategoryColor::DarkMaroon,
    ];

    pub fn rgb(self) -> Color {
        match self {
            CategoryColor::None => Color::TRANSPARENT,
            CategoryColor::Red => Color::hex(0xE5484D),
            CategoryColor::Orange => Color::hex(0xF28A2E),
            CategoryColor::Peach => Color::hex(0xF7B98B),
            CategoryColor::Yellow => Color::hex(0xF5D547),
            CategoryColor::Green => Color::hex(0x5FBF5A),
            CategoryColor::Teal => Color::hex(0x3EB8A9),
            CategoryColor::Olive => Color::hex(0x9CA95A),
            CategoryColor::Blue => Color::hex(0x4A8FE0),
            CategoryColor::Purple => Color::hex(0x9B6BD6),
            CategoryColor::Maroon => Color::hex(0xC05C82),
            CategoryColor::Steel => Color::hex(0x9AB3C2),
            CategoryColor::DarkSteel => Color::hex(0x5B7486),
            CategoryColor::Gray => Color::hex(0xB4B4B4),
            CategoryColor::DarkGray => Color::hex(0x6E6E6E),
            CategoryColor::Black => Color::hex(0x2B2B2B),
            CategoryColor::DarkRed => Color::hex(0xA32A2E),
            CategoryColor::DarkOrange => Color::hex(0xB85A12),
            CategoryColor::DarkPeach => Color::hex(0xC47E4F),
            CategoryColor::DarkYellow => Color::hex(0xB89A17),
            CategoryColor::DarkGreen => Color::hex(0x2F7D32),
            CategoryColor::DarkTeal => Color::hex(0x1C7A70),
            CategoryColor::DarkOlive => Color::hex(0x677034),
            CategoryColor::DarkBlue => Color::hex(0x1F4F99),
            CategoryColor::DarkPurple => Color::hex(0x5E3A92),
            CategoryColor::DarkMaroon => Color::hex(0x802D50),
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            CategoryColor::None => "None",
            CategoryColor::Red => "Red",
            CategoryColor::Orange => "Orange",
            CategoryColor::Peach => "Peach",
            CategoryColor::Yellow => "Yellow",
            CategoryColor::Green => "Green",
            CategoryColor::Teal => "Teal",
            CategoryColor::Olive => "Olive",
            CategoryColor::Blue => "Blue",
            CategoryColor::Purple => "Purple",
            CategoryColor::Maroon => "Maroon",
            CategoryColor::Steel => "Steel",
            CategoryColor::DarkSteel => "Dark Steel",
            CategoryColor::Gray => "Gray",
            CategoryColor::DarkGray => "Dark Gray",
            CategoryColor::Black => "Black",
            CategoryColor::DarkRed => "Dark Red",
            CategoryColor::DarkOrange => "Dark Orange",
            CategoryColor::DarkPeach => "Dark Peach",
            CategoryColor::DarkYellow => "Dark Yellow",
            CategoryColor::DarkGreen => "Dark Green",
            CategoryColor::DarkTeal => "Dark Teal",
            CategoryColor::DarkOlive => "Dark Olive",
            CategoryColor::DarkBlue => "Dark Blue",
            CategoryColor::DarkPurple => "Dark Purple",
            CategoryColor::DarkMaroon => "Dark Maroon",
        }
    }
    /// Inverse of [`CategoryColor::name`] (case-insensitive, spaces optional).
    pub fn from_name(name: &str) -> Option<CategoryColor> {
        let key: String = name.chars().filter(|c| !c.is_whitespace()).collect::<String>().to_lowercase();
        Self::ALL.into_iter().find(|c| c.name().replace(' ', "").to_lowercase() == key)
    }
    /// Readable text colour (black or white) on top of this colour's fill.
    pub fn text_color(self) -> Color {
        let c = self.rgb();
        if c.a == 0 {
            return Color::BLACK;
        }
        // Rec. 601 luma, integer arithmetic.
        let luma = u32::from(c.r) * 299 + u32::from(c.g) * 587 + u32::from(c.b) * 114;
        if luma >= 140_000 { Color::BLACK } else { Color::WHITE }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Category {
    pub name: String,
    pub color: CategoryColor,
    /// "Ctrl+F2" … "Ctrl+F12", or empty.
    pub shortcut: String,
}

impl Category {
    /// The six default categories every new mailbox gets ("Red category", "Orange category", … "Purple category").
    pub fn defaults() -> Vec<Category> {
        [
            CategoryColor::Red,
            CategoryColor::Orange,
            CategoryColor::Yellow,
            CategoryColor::Green,
            CategoryColor::Blue,
            CategoryColor::Purple,
        ]
        .into_iter()
        .map(|color| Category { name: format!("{} category", color.name()), color, shortcut: String::new() })
        .collect()
    }
    /// Finds a category by name (case-insensitive, as category names are matched in rules and search).
    pub fn find<'a>(list: &'a [Category], name: &str) -> Option<&'a Category> {
        let name = name.trim();
        let key = name.to_lowercase();
        list.iter().find(|c| c.name.trim().to_lowercase() == key)
    }
}
