//! Insert › Icons: a bundled set of simple pictograms drawn for MailCraft (48×48 SVG bodies, plain shapes
//! only), sorted into categories. They are inserted as PNG (rasterised by `mailcraft-render`), since many
//! mail programs do not show SVG.

/// Icon categories, in the order the Insert Icons dialog shows them.
pub const CATEGORIES: [&str; 8] =
    ["Interface", "Communication", "Arrows", "Time", "People", "Objects", "Business", "Nature"];

/// Every icon: (id, label, category, 48×48 SVG body).
pub const ICONS: &[(&str, &str, &str, &str)] = &[
    (
        "check",
        "Check mark",
        "Interface",
        r##"<path d="M8 26 L20 38 L40 12" fill="none" stroke="#196b24" stroke-width="6" stroke-linecap="round" stroke-linejoin="round"/>"##,
    ),
    (
        "cross",
        "Cross",
        "Interface",
        r##"<path d="M12 12 L36 36 M36 12 L12 36" stroke="#c00000" stroke-width="6" stroke-linecap="round"/>"##,
    ),
    (
        "plus",
        "Plus",
        "Interface",
        r##"<path d="M24 8 V40 M8 24 H40" stroke="#196b24" stroke-width="6" stroke-linecap="round"/>"##,
    ),
    (
        "minus",
        "Minus",
        "Interface",
        r##"<path d="M8 24 H40" stroke="#c00000" stroke-width="6" stroke-linecap="round"/>"##,
    ),
    (
        "info",
        "Information",
        "Interface",
        r##"<circle cx="24" cy="24" r="21" fill="#0f6cbd"/><path d="M24 21 V35 M24 13 V15" stroke="#fff" stroke-width="5" stroke-linecap="round"/>"##,
    ),
    (
        "warning",
        "Warning",
        "Interface",
        r##"<path d="M24 4 L46 44 H2 Z" fill="#f2c811"/><path d="M24 18 V30 M24 36 V38" stroke="#222" stroke-width="4" stroke-linecap="round"/>"##,
    ),
    (
        "question",
        "Question",
        "Interface",
        r##"<circle cx="24" cy="24" r="21" fill="#5b5fc7"/><path d="M18 18 C18 10 30 10 30 18 C30 23 24 23 24 29" fill="none" stroke="#fff" stroke-width="4" stroke-linecap="round"/><circle cx="24" cy="36" r="2.5" fill="#fff"/>"##,
    ),
    (
        "search",
        "Search",
        "Interface",
        r##"<circle cx="20" cy="20" r="12" fill="none" stroke="#424242" stroke-width="4"/><path d="M29 29 L42 42" stroke="#424242" stroke-width="5" stroke-linecap="round"/>"##,
    ),
    (
        "settings",
        "Settings",
        "Interface",
        r##"<circle cx="24" cy="24" r="13" fill="#616161"/><g fill="#616161"><rect x="20" y="3" width="8" height="10" rx="1"/><rect x="20" y="35" width="8" height="10" rx="1"/><rect x="3" y="20" width="10" height="8" rx="1"/><rect x="35" y="20" width="10" height="8" rx="1"/><g transform="rotate(45 24 24)"><rect x="20" y="3" width="8" height="10" rx="1"/><rect x="20" y="35" width="8" height="10" rx="1"/><rect x="3" y="20" width="10" height="8" rx="1"/><rect x="35" y="20" width="10" height="8" rx="1"/></g></g><circle cx="24" cy="24" r="5" fill="#fff"/>"##,
    ),
    (
        "lock",
        "Lock",
        "Interface",
        r##"<path d="M16 21 V15 A8 8 0 0 1 32 15 V21" fill="none" stroke="#616161" stroke-width="4"/><rect x="10" y="21" width="28" height="22" rx="3" fill="#c19c00"/><circle cx="24" cy="31" r="3" fill="#fff"/>"##,
    ),
    (
        "unlock",
        "Unlocked",
        "Interface",
        r##"<path d="M16 21 V15 A8 8 0 0 1 31 11" fill="none" stroke="#616161" stroke-width="4"/><rect x="10" y="21" width="28" height="22" rx="3" fill="#c19c00"/><circle cx="24" cy="31" r="3" fill="#fff"/>"##,
    ),
    (
        "key",
        "Key",
        "Interface",
        r##"<circle cx="14" cy="24" r="9" fill="none" stroke="#c19c00" stroke-width="5"/><path d="M23 24 H44 M36 24 V31 M42 24 V29" stroke="#c19c00" stroke-width="5" stroke-linecap="round"/>"##,
    ),
    (
        "bell",
        "Bell",
        "Interface",
        r##"<path d="M24 5 C15 5 12 13 12 20 V30 L7 36 H41 L36 30 V20 C36 13 33 5 24 5 Z" fill="#f2c811"/><circle cx="24" cy="40" r="4" fill="#616161"/>"##,
    ),
    (
        "pin",
        "Location pin",
        "Interface",
        r##"<path d="M24 45 C24 45 9 28 9 18 A15 15 0 0 1 39 18 C39 28 24 45 24 45 Z" fill="#c00000"/><circle cx="24" cy="18" r="6" fill="#fff"/>"##,
    ),
    (
        "trash",
        "Waste bin",
        "Interface",
        r##"<rect x="9" y="9" width="30" height="5" rx="1" fill="#616161"/><rect x="19" y="4" width="10" height="5" rx="1" fill="#616161"/><path d="M12 16 H36 L33 44 H15 Z" fill="#9e9e9e"/><path d="M20 21 V39 M28 21 V39" stroke="#fff" stroke-width="2.5"/>"##,
    ),
    (
        "pencil",
        "Pencil",
        "Interface",
        r##"<path d="M8 40 L10 31 L33 8 L40 15 L17 38 Z" fill="#f7a800"/><path d="M8 40 L10 31 L17 38 Z" fill="#424242"/><path d="M30 11 L37 18" stroke="#c00000" stroke-width="4"/>"##,
    ),
    (
        "link",
        "Link",
        "Interface",
        r##"<rect x="4" y="17" width="24" height="14" rx="7" fill="none" stroke="#0f6cbd" stroke-width="4"/><rect x="20" y="17" width="24" height="14" rx="7" fill="none" stroke="#0f6cbd" stroke-width="4"/>"##,
    ),
    (
        "paperclip",
        "Paper clip",
        "Interface",
        r##"<path d="M30 14 V34 A7 7 0 0 1 16 34 V12 A5 5 0 0 1 26 12 V32 A2 2 0 0 1 22 32 V16" fill="none" stroke="#616161" stroke-width="3" stroke-linecap="round"/>"##,
    ),
    (
        "download",
        "Download",
        "Interface",
        r##"<path d="M24 6 V30 M14 21 L24 31 L34 21" fill="none" stroke="#0f6cbd" stroke-width="5" stroke-linecap="round" stroke-linejoin="round"/><path d="M8 38 H40" stroke="#424242" stroke-width="5" stroke-linecap="round"/>"##,
    ),
    (
        "upload",
        "Upload",
        "Interface",
        r##"<path d="M24 32 V8 M14 17 L24 7 L34 17" fill="none" stroke="#0f6cbd" stroke-width="5" stroke-linecap="round" stroke-linejoin="round"/><path d="M8 38 H40" stroke="#424242" stroke-width="5" stroke-linecap="round"/>"##,
    ),
    (
        "star",
        "Star",
        "Interface",
        r##"<polygon points="24,3 30,18 46,18 33,28 38,44 24,34 10,44 15,28 2,18 18,18" fill="#e9a800"/>"##,
    ),
    (
        "flag",
        "Flag",
        "Interface",
        r##"<path d="M10 4 V44" stroke="#424242" stroke-width="3"/><path d="M11 6 H40 L33 15 L40 24 H11 Z" fill="#c00000"/>"##,
    ),
    (
        "mail",
        "Envelope",
        "Communication",
        r##"<rect x="3" y="10" width="42" height="28" rx="3" fill="#0f6cbd"/><path d="M5 13 L24 27 L43 13" fill="none" stroke="#fff" stroke-width="3"/>"##,
    ),
    (
        "phone",
        "Mobile phone",
        "Communication",
        r##"<rect x="14" y="3" width="20" height="42" rx="4" fill="#424242"/><rect x="17" y="8" width="14" height="28" fill="#9fd3ff"/><circle cx="24" cy="40" r="2" fill="#fff"/>"##,
    ),
    (
        "chat",
        "Chat bubble",
        "Communication",
        r##"<path d="M6 8 H42 V32 H20 L11 41 V32 H6 Z" fill="#5b5fc7"/><path d="M13 17 H35 M13 24 H28" stroke="#fff" stroke-width="3" stroke-linecap="round"/>"##,
    ),
    (
        "send",
        "Paper plane",
        "Communication",
        r##"<path d="M4 23 L44 6 L35 42 L23 31 Z" fill="#0f6cbd"/><path d="M23 31 L44 6 L19 27 Z" fill="#0b4f8a"/>"##,
    ),
    (
        "megaphone",
        "Megaphone",
        "Communication",
        r##"<path d="M8 19 H16 L36 8 V40 L16 29 H8 Z" fill="#e97132"/><path d="M14 29 L18 41 H24 L21 29" fill="#616161"/>"##,
    ),
    (
        "at",
        "At sign",
        "Communication",
        r##"<circle cx="24" cy="24" r="8" fill="none" stroke="#0f6cbd" stroke-width="4"/><path d="M32 18 V27 A5 5 0 0 0 42 27 V24 A18 18 0 1 0 34 39" fill="none" stroke="#0f6cbd" stroke-width="4" stroke-linecap="round"/>"##,
    ),
    (
        "globe",
        "Globe",
        "Communication",
        r##"<circle cx="24" cy="24" r="19" fill="#0f9ed5"/><path d="M5 24 H43 M24 5 C14 14 14 34 24 43 M24 5 C34 14 34 34 24 43" fill="none" stroke="#fff" stroke-width="2.5"/>"##,
    ),
    (
        "arrowRight",
        "Arrow right",
        "Arrows",
        r##"<path d="M4 24 H40 M28 12 L40 24 L28 36" fill="none" stroke="#0f6cbd" stroke-width="5" stroke-linecap="round" stroke-linejoin="round"/>"##,
    ),
    (
        "arrowLeft",
        "Arrow left",
        "Arrows",
        r##"<path d="M44 24 H8 M20 12 L8 24 L20 36" fill="none" stroke="#0f6cbd" stroke-width="5" stroke-linecap="round" stroke-linejoin="round"/>"##,
    ),
    (
        "arrowUp",
        "Arrow up",
        "Arrows",
        r##"<path d="M24 44 V8 M12 20 L24 8 L36 20" fill="none" stroke="#0f6cbd" stroke-width="5" stroke-linecap="round" stroke-linejoin="round"/>"##,
    ),
    (
        "arrowDown",
        "Arrow down",
        "Arrows",
        r##"<path d="M24 4 V40 M12 28 L24 40 L36 28" fill="none" stroke="#0f6cbd" stroke-width="5" stroke-linecap="round" stroke-linejoin="round"/>"##,
    ),
    (
        "refresh",
        "Circular arrow",
        "Arrows",
        r##"<path d="M38 22 A14 14 0 1 0 34 34" fill="none" stroke="#196b24" stroke-width="5" stroke-linecap="round"/><path d="M30 20 H40 V10" fill="none" stroke="#196b24" stroke-width="5" stroke-linecap="round" stroke-linejoin="round"/>"##,
    ),
    (
        "calendar",
        "Calendar",
        "Time",
        r##"<rect x="4" y="8" width="40" height="36" rx="3" fill="#fff" stroke="#0f6cbd" stroke-width="3"/><rect x="4" y="8" width="40" height="10" fill="#0f6cbd"/><path d="M14 4 V12 M34 4 V12" stroke="#0f6cbd" stroke-width="3"/>"##,
    ),
    (
        "clock",
        "Clock",
        "Time",
        r##"<circle cx="24" cy="24" r="20" fill="#fff" stroke="#424242" stroke-width="3"/><path d="M24 12 V24 L32 30" fill="none" stroke="#424242" stroke-width="3" stroke-linecap="round"/>"##,
    ),
    (
        "hourglass",
        "Hourglass",
        "Time",
        r##"<path d="M14 6 H34 C34 16 26 20 26 24 C26 28 34 32 34 42 H14 C14 32 22 28 22 24 C22 20 14 16 14 6 Z" fill="#f2c811"/><path d="M12 4 H36 M12 44 H36" stroke="#616161" stroke-width="4" stroke-linecap="round"/>"##,
    ),
    (
        "alarm",
        "Alarm clock",
        "Time",
        r##"<circle cx="24" cy="26" r="16" fill="#fff" stroke="#c00000" stroke-width="4"/><path d="M24 17 V26 L30 30" fill="none" stroke="#424242" stroke-width="3" stroke-linecap="round"/><path d="M6 12 L13 5 M42 12 L35 5" stroke="#c00000" stroke-width="4" stroke-linecap="round"/>"##,
    ),
    (
        "person",
        "Person",
        "People",
        r##"<circle cx="24" cy="15" r="9" fill="#5b5fc7"/><path d="M6 44 C6 32 14 27 24 27 C34 27 42 32 42 44 Z" fill="#5b5fc7"/>"##,
    ),
    (
        "people",
        "People",
        "People",
        r##"<circle cx="32" cy="14" r="7" fill="#8f93e0"/><path d="M20 40 C20 30 26 25 32 25 C40 25 46 30 46 40 Z" fill="#8f93e0"/><circle cx="18" cy="17" r="8" fill="#5b5fc7"/><path d="M2 44 C2 33 9 29 18 29 C27 29 34 33 34 44 Z" fill="#5b5fc7"/>"##,
    ),
    (
        "smile",
        "Smiling face",
        "People",
        r##"<circle cx="24" cy="24" r="20" fill="#f2c811"/><circle cx="17" cy="19" r="3" fill="#424242"/><circle cx="31" cy="19" r="3" fill="#424242"/><path d="M14 28 C18 36 30 36 34 28" fill="none" stroke="#424242" stroke-width="3.5" stroke-linecap="round"/>"##,
    ),
    (
        "thumbsUp",
        "Thumbs up",
        "People",
        r##"<path d="M6 22 H14 V42 H6 Z" fill="#0f6cbd"/><path d="M16 22 L24 6 C28 6 30 9 29 14 L28 19 H40 C43 19 44 22 43 25 L39 39 C38 41 37 42 35 42 H16 Z" fill="#0f6cbd"/>"##,
    ),
    (
        "heart",
        "Heart",
        "People",
        r##"<path d="M24 42 C8 30 2 20 8 12 C14 4 22 8 24 14 C26 8 34 4 40 12 C46 20 40 30 24 42 Z" fill="#d13438"/>"##,
    ),
    (
        "home",
        "House",
        "Objects",
        r##"<path d="M4 24 L24 6 L44 24 H38 V42 H10 V24 Z" fill="#196b24"/><rect x="20" y="30" width="8" height="12" fill="#fff"/>"##,
    ),
    (
        "lightbulb",
        "Light bulb",
        "Objects",
        r##"<path d="M24 4 C14 4 9 12 9 18 C9 26 16 29 17 35 H31 C32 29 39 26 39 18 C39 12 34 4 24 4 Z" fill="#f2c811"/><rect x="17" y="37" width="14" height="7" rx="2" fill="#616161"/>"##,
    ),
    (
        "document",
        "Document",
        "Objects",
        r##"<path d="M10 3 H30 L40 13 V45 H10 Z" fill="#fff" stroke="#616161" stroke-width="2.5"/><path d="M30 3 V13 H40" fill="none" stroke="#616161" stroke-width="2.5"/><path d="M16 22 H34 M16 29 H34 M16 36 H28" stroke="#9e9e9e" stroke-width="2.5"/>"##,
    ),
    (
        "folder",
        "Folder",
        "Objects",
        r##"<path d="M4 12 H18 L22 16 H44 V40 H4 Z" fill="#f2c811"/><path d="M4 20 H44" stroke="#c19c00" stroke-width="2"/>"##,
    ),
    (
        "camera",
        "Camera",
        "Objects",
        r##"<path d="M16 13 L19 7 H29 L32 13 Z" fill="#424242"/><rect x="4" y="13" width="40" height="28" rx="4" fill="#424242"/><circle cx="24" cy="27" r="9" fill="#9fd3ff"/><circle cx="24" cy="27" r="5" fill="#0f6cbd"/>"##,
    ),
    (
        "image",
        "Picture",
        "Objects",
        r##"<rect x="4" y="8" width="40" height="32" rx="3" fill="#9fd3ff"/><path d="M4 34 L16 22 L26 32 L32 26 L44 38 V40 H4 Z" fill="#196b24"/><circle cx="33" cy="16" r="4" fill="#f2c811"/>"##,
    ),
    (
        "gift",
        "Gift",
        "Objects",
        r##"<rect x="6" y="18" width="36" height="24" fill="#c00000"/><rect x="4" y="12" width="40" height="8" fill="#e04848"/><rect x="21" y="12" width="6" height="30" fill="#f2c811"/><path d="M24 12 C16 2 10 8 16 12 M24 12 C32 2 38 8 32 12" fill="none" stroke="#f2c811" stroke-width="3"/>"##,
    ),
    (
        "music",
        "Music notes",
        "Objects",
        r##"<path d="M18 36 V10 L40 5 V31" fill="none" stroke="#5b5fc7" stroke-width="4"/><ellipse cx="13" cy="37" rx="6" ry="5" fill="#5b5fc7"/><ellipse cx="35" cy="32" rx="6" ry="5" fill="#5b5fc7"/>"##,
    ),
    (
        "coffee",
        "Hot drink",
        "Objects",
        r##"<path d="M8 16 H34 V32 A10 10 0 0 1 24 42 H18 A10 10 0 0 1 8 32 Z" fill="#7a4a24"/><path d="M34 20 H38 A5 5 0 0 1 38 30 H34" fill="none" stroke="#7a4a24" stroke-width="3"/><path d="M16 4 C14 8 18 9 16 12 M24 4 C22 8 26 9 24 12" fill="none" stroke="#9e9e9e" stroke-width="2.5" stroke-linecap="round"/>"##,
    ),
    (
        "car",
        "Car",
        "Objects",
        r##"<path d="M6 30 L10 18 C11 15 13 14 16 14 H32 C35 14 37 15 38 18 L42 30 V38 H6 Z" fill="#0f6cbd"/><path d="M13 26 L15 19 H33 L35 26 Z" fill="#9fd3ff"/><circle cx="14" cy="38" r="5" fill="#424242"/><circle cx="34" cy="38" r="5" fill="#424242"/>"##,
    ),
    (
        "plane",
        "Aeroplane",
        "Objects",
        r##"<path d="M22 4 C24 2 26 4 26 6 V18 L44 28 V32 L26 26 V38 L32 42 V45 L24 43 L16 45 V42 L22 38 V26 L4 32 V28 L22 18 Z" fill="#616161"/>"##,
    ),
    (
        "chart",
        "Bar chart",
        "Business",
        r##"<path d="M6 6 V42 H44" fill="none" stroke="#424242" stroke-width="3"/><rect x="12" y="26" width="7" height="14" fill="#0f6cbd"/><rect x="23" y="16" width="7" height="24" fill="#e97132"/><rect x="34" y="10" width="7" height="30" fill="#196b24"/>"##,
    ),
    (
        "trophy",
        "Trophy",
        "Business",
        r##"<path d="M14 9 H7 C7 18 11 20 15 21 M34 9 H41 C41 18 37 20 33 21" fill="none" stroke="#c19c00" stroke-width="3"/><path d="M14 6 H34 V18 A10 10 0 0 1 14 18 Z" fill="#f2c811"/><rect x="21" y="27" width="6" height="9" fill="#c19c00"/><rect x="14" y="36" width="20" height="6" rx="1" fill="#616161"/>"##,
    ),
    (
        "money",
        "Coin",
        "Business",
        r##"<circle cx="24" cy="24" r="20" fill="#196b24"/><path d="M30 16 C28 13 18 12 18 18 C18 24 30 22 30 29 C30 35 20 35 17 31 M24 10 V38" fill="none" stroke="#fff" stroke-width="3.5" stroke-linecap="round"/>"##,
    ),
    (
        "briefcase",
        "Briefcase",
        "Business",
        r##"<path d="M17 14 V9 H31 V14" fill="none" stroke="#7a4a24" stroke-width="3.5"/><rect x="4" y="14" width="40" height="28" rx="3" fill="#7a4a24"/><rect x="4" y="24" width="40" height="3" fill="#5c3518"/>"##,
    ),
    (
        "target",
        "Target",
        "Business",
        r##"<circle cx="24" cy="24" r="20" fill="#c00000"/><circle cx="24" cy="24" r="14" fill="#fff"/><circle cx="24" cy="24" r="8" fill="#c00000"/><circle cx="24" cy="24" r="3" fill="#fff"/>"##,
    ),
    (
        "rocket",
        "Rocket",
        "Business",
        r##"<path d="M24 3 C32 10 34 20 32 32 H16 C14 20 16 10 24 3 Z" fill="#9e9e9e"/><circle cx="24" cy="17" r="4" fill="#0f6cbd"/><path d="M16 26 L9 34 V40 L16 34 Z M32 26 L39 34 V40 L32 34 Z" fill="#c00000"/><path d="M19 32 L24 44 L29 32 Z" fill="#e97132"/>"##,
    ),
    (
        "sun",
        "Sun",
        "Nature",
        r##"<circle cx="24" cy="24" r="9" fill="#f2c811"/><path d="M24 4 V10 M24 38 V44 M4 24 H10 M38 24 H44 M10 10 L14 14 M34 34 L38 38 M38 10 L34 14 M10 38 L14 34" stroke="#f2c811" stroke-width="4" stroke-linecap="round"/>"##,
    ),
    (
        "moon",
        "Moon",
        "Nature",
        r##"<path d="M30 6 A18 18 0 1 0 42 32 A14 14 0 1 1 30 6 Z" fill="#5b5fc7"/>"##,
    ),
    (
        "cloud",
        "Cloud",
        "Nature",
        r##"<path d="M14 38 A9 9 0 0 1 13 20 A12 12 0 0 1 36 18 A10 10 0 0 1 36 38 Z" fill="#9fd3ff"/>"##,
    ),
    (
        "leaf",
        "Leaf",
        "Nature",
        r##"<path d="M8 40 C8 16 22 6 42 6 C42 26 32 40 8 40 Z" fill="#4ea72e"/><path d="M8 40 L32 16" stroke="#196b24" stroke-width="3"/>"##,
    ),
    (
        "umbrella",
        "Umbrella",
        "Nature",
        r##"<path d="M4 24 A20 20 0 0 1 44 24 Z" fill="#c00000"/><path d="M24 24 V40 A4 4 0 0 1 16 40" fill="none" stroke="#424242" stroke-width="3"/>"##,
    ),
    (
        "fire",
        "Flame",
        "Nature",
        r##"<path d="M24 44 C12 44 8 34 12 26 C14 22 16 22 17 17 C21 21 21 24 21 26 C24 20 24 12 22 4 C32 10 40 20 38 32 C37 39 31 44 24 44 Z" fill="#e97132"/>"##,
    ),
    (
        "tree",
        "Tree",
        "Nature",
        r##"<rect x="21" y="32" width="6" height="12" fill="#7a4a24"/><path d="M24 3 L40 24 H32 L42 36 H6 L16 24 H8 Z" fill="#196b24"/>"##,
    ),
];

/// The icon called `name` (id or label, case-insensitive).
pub fn find(name: &str) -> Option<&'static (&'static str, &'static str, &'static str, &'static str)> {
    let n = name.trim();
    ICONS.iter().find(|(id, label, ..)| id.eq_ignore_ascii_case(n) || label.eq_ignore_ascii_case(n))
}

/// The full SVG of icon `id`.
pub fn svg(id: &str) -> Option<String> {
    find(id).map(|(.., body)| {
        format!(r#"<svg xmlns="http://www.w3.org/2000/svg" width="48" height="48" viewBox="0 0 48 48">{body}</svg>"#)
    })
}

/// Icon `id` as a PNG `px` pixels square (`None` for an unknown id).
pub fn png(id: &str, px: f32) -> Option<Vec<u8>> {
    let px = if px.is_finite() { px.clamp(8.0, 1024.0) } else { 48.0 };
    mailcraft_render::svg::svg_to_png(svg(id)?.as_bytes(), px / 48.0).map(|p| p.png)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_icon_draws_and_ids_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for (id, label, cat, _) in ICONS {
            assert!(seen.insert(*id), "{id} twice");
            assert!(CATEGORIES.contains(cat), "{id}: {cat}");
            assert!(!label.is_empty());
            let png = png(id, 32.0).unwrap_or_else(|| panic!("{id} does not draw"));
            let pm = mailcraft_render::svg::decode_png(&png).unwrap();
            assert_eq!((pm.width, pm.height), (32, 32));
            assert!(pm.pixels.chunks(4).any(|p| p[3] > 0), "{id} is blank");
        }
        assert!(ICONS.len() >= 60, "{}", ICONS.len());
        for cat in CATEGORIES {
            assert!(ICONS.iter().any(|i| i.2 == cat), "{cat} is empty");
        }
        assert_eq!(find("Light bulb").map(|i| i.0), Some("lightbulb"));
        assert!(png("nope", 32.0).is_none());
    }
}
