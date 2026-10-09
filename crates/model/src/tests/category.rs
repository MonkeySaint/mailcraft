use std::collections::HashSet;

use crate::quickstep::QuickStepAction;
use crate::{Category, CategoryColor, QuickStep};

#[test]
fn every_colour_has_a_unique_name_and_rgb() {
    let names: HashSet<&str> = CategoryColor::ALL.iter().map(|c| c.name()).collect();
    assert_eq!(names.len(), 26);
    let rgbs: HashSet<_> = CategoryColor::ALL.iter().map(|c| c.rgb()).collect();
    assert_eq!(rgbs.len(), 26);
    assert_eq!(CategoryColor::None.rgb().a, 0);
    for c in CategoryColor::ALL.iter().skip(1) {
        assert_eq!(c.rgb().a, 255, "{c:?} opaque");
    }
}

#[test]
fn colour_names() {
    assert_eq!(CategoryColor::Red.name(), "Red");
    assert_eq!(CategoryColor::Peach.name(), "Peach");
    assert_eq!(CategoryColor::DarkSteel.name(), "Dark Steel");
    assert_eq!(CategoryColor::DarkGray.name(), "Dark Gray");
    assert_eq!(CategoryColor::DarkRed.name(), "Dark Red");
    assert_eq!(CategoryColor::DarkMaroon.name(), "Dark Maroon");
    assert_eq!(CategoryColor::None.name(), "None");
}

#[test]
fn colour_from_name_round_trips() {
    for c in CategoryColor::ALL {
        assert_eq!(CategoryColor::from_name(c.name()), Some(c));
        assert_eq!(CategoryColor::from_name(&c.name().to_uppercase()), Some(c));
    }
    assert_eq!(CategoryColor::from_name("darkblue"), Some(CategoryColor::DarkBlue));
    assert_eq!(CategoryColor::from_name("chartreuse"), None);
}

#[test]
fn dark_colours_are_darker_than_light_ones() {
    let luma = |c: CategoryColor| {
        let k = c.rgb();
        u32::from(k.r) * 299 + u32::from(k.g) * 587 + u32::from(k.b) * 114
    };
    let pairs = [
        (CategoryColor::Red, CategoryColor::DarkRed),
        (CategoryColor::Orange, CategoryColor::DarkOrange),
        (CategoryColor::Peach, CategoryColor::DarkPeach),
        (CategoryColor::Yellow, CategoryColor::DarkYellow),
        (CategoryColor::Green, CategoryColor::DarkGreen),
        (CategoryColor::Teal, CategoryColor::DarkTeal),
        (CategoryColor::Olive, CategoryColor::DarkOlive),
        (CategoryColor::Blue, CategoryColor::DarkBlue),
        (CategoryColor::Purple, CategoryColor::DarkPurple),
        (CategoryColor::Maroon, CategoryColor::DarkMaroon),
        (CategoryColor::Steel, CategoryColor::DarkSteel),
        (CategoryColor::Gray, CategoryColor::DarkGray),
    ];
    for (light, dark) in pairs {
        assert!(luma(dark) < luma(light), "{dark:?} darker than {light:?}");
    }
}

#[test]
fn text_colour_contrasts() {
    assert_eq!(CategoryColor::Yellow.text_color(), mailcraft_geom::Color::BLACK);
    assert_eq!(CategoryColor::Black.text_color(), mailcraft_geom::Color::WHITE);
    assert_eq!(CategoryColor::DarkBlue.text_color(), mailcraft_geom::Color::WHITE);
}

#[test]
fn default_categories() {
    let d = Category::defaults();
    let names: Vec<&str> = d.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "Red category",
            "Orange category",
            "Yellow category",
            "Green category",
            "Blue category",
            "Purple category"
        ]
    );
    let colours: Vec<CategoryColor> = d.iter().map(|c| c.color).collect();
    assert_eq!(
        colours,
        [
            CategoryColor::Red,
            CategoryColor::Orange,
            CategoryColor::Yellow,
            CategoryColor::Green,
            CategoryColor::Blue,
            CategoryColor::Purple
        ]
    );
    assert!(d.iter().all(|c| c.shortcut.is_empty()));
    assert_eq!(Category::find(&d, "blue CATEGORY").map(|c| c.color), Some(CategoryColor::Blue));
    assert!(Category::find(&d, "Teal category").is_none());
}

#[test]
fn default_quick_steps() {
    let q = QuickStep::defaults();
    let names: Vec<&str> = q.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["Move to: ?", "To Manager", "Team Email", "Done", "Reply & Delete", "Create New"]);
    let ids: HashSet<u64> = q.iter().map(|s| s.id).collect();
    assert_eq!(ids.len(), 6);
    assert!(q.iter().all(|s| !s.tooltip.is_empty()));
    assert!(q[0].actions.contains(&QuickStepAction::MoveTo(None)));
    assert_eq!(q[4].actions, vec![QuickStepAction::Reply, QuickStepAction::Delete]);
    assert!(q[0].needs_setup());
    assert!(q[1].needs_setup());
    assert!(!q[4].needs_setup());
    assert!(q[5].needs_setup());
}
