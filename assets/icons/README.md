# MailCraft icons

Every SVG in this directory is original MailCraft artwork, drawn for this project from the meaning of each
command (an envelope for mail, a pennant on a pole for a flag, a box with a lid for archive, and so on). None
of it is traced from, derived from or modelled on Microsoft, Fluent or Office icons, and no third-party icon
set is included.

Licence: MIT OR Apache-2.0, the same as the rest of MailCraft.

## Style

- 24 x 24 `viewBox`, 1.75 px strokes, round caps and joins, `fill="none"`.
- Monochrome: everything is drawn in `currentColor`. The app replaces it with white when it rasterises an
  icon and tints the texture with the theme colour (`crates/ui-egui/src/icons.rs`).
- No `<text>`, no external references, no embedded raster images. Badges (the plus on "new-mail", the clock
  on "appointment") sit in a circle cut out of the base shape with a `<mask>`.

## Adding an icon

1. Add `name.svg` here in the same style.
2. Add `"name"` to the `icon_table!` list in `crates/ui-egui/src/icons.rs`, keeping alphabetical order.
3. Run `cargo test -p mailcraft-ui-egui icons`. The tests check that the table matches this directory and
   that every file parses and renders.
