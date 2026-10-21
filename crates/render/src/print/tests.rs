use mailcraft_fonts::FontDb;
use mailcraft_geom::Color;
use mailcraft_layout::DisplayItem;
use mailcraft_model::{Block, Doc};

use super::*;
use crate::pdf::{PdfInfo, write_pdf};

fn fonts() -> FontDb {
    FontDb::bundled()
}

fn ctx() -> PrintContext {
    PrintContext {
        user_name: "Pat Example".into(),
        date_printed: "10/10/2026".into(),
        time_printed: "9:00 AM".into(),
        title: "Test".into(),
    }
}

fn texts(p: &Page) -> Vec<&str> {
    p.display
        .items
        .iter()
        .filter_map(|i| match i {
            DisplayItem::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

fn memo(paras: usize) -> MemoItem {
    let blocks = (0..paras)
        .map(|i| {
            Block::text_paragraph(&format!("Paragraph {i}: the quick brown fox jumps over the lazy dog."))
        })
        .collect();
    MemoItem {
        fields: vec![
            ("From".into(), "Alex <alex@example.com>".into()),
            ("Sent".into(), "Friday, October 9, 2026 10:00 AM".into()),
            ("To".into(), "Pat <pat@example.org>".into()),
            ("Subject".into(), "Quarterly numbers".into()),
            ("Attachments".into(), "report.pdf".into()),
        ],
        body: Doc { blocks, ..Doc::default() },
    }
}

// ------------------------------------------------------------------ minimal PDF parser for the tests

/// Checks the file structure: header, trailer, `startxref` → classic xref table whose in-use entries point at
/// `N 0 obj` lines. Returns (objects in the xref, page objects).
fn parse_pdf(b: &[u8]) -> (usize, usize) {
    assert!(b.starts_with(b"%PDF-1."), "header");
    let tail = &b[b.len().saturating_sub(64)..];
    let tail_s = String::from_utf8_lossy(tail);
    assert!(tail_s.trim_end().ends_with("%%EOF"), "EOF marker");
    let sx = tail_s.rfind("startxref").expect("startxref");
    let off: usize = tail_s[sx + 9..].split_whitespace().next().unwrap().parse().unwrap();
    let xref = &b[off..];
    assert!(xref.starts_with(b"xref"), "startxref points at the xref table");
    let s = String::from_utf8_lossy(&xref[..xref.len().min(4_000_000)]).into_owned();
    let mut lines = s.lines();
    lines.next();
    let sub: Vec<usize> = lines.next().unwrap().split_whitespace().map(|v| v.parse().unwrap()).collect();
    assert_eq!(sub[0], 0);
    let count = sub[1];
    let mut used = 0;
    for (n, line) in lines.take(count).enumerate() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        assert_eq!(parts.len(), 3, "xref entry {n}: {line:?}");
        if parts[2] == "n" {
            let at: usize = parts[0].parse().unwrap();
            let head = format!("{n} 0 obj");
            assert!(b[at..].starts_with(head.as_bytes()), "object {n} at {at}");
            used += 1;
        }
    }
    assert!(s.contains("trailer"), "trailer");
    let all = String::from_utf8_lossy(b);
    let obj_count = all.matches(" 0 obj").count();
    assert_eq!(obj_count, used, "every object is in the xref");
    // `/Type /Page` dictionaries (with or without the space; not `/Pages`).
    let mut pages = 0;
    for (i, _) in all.match_indices("/Type") {
        let rest = all[i + 5..].trim_start();
        if rest.starts_with("/Page") && !rest.starts_with("/Pages") {
            pages += 1;
        }
    }
    (used, pages)
}

// ------------------------------------------------------------------ memo

#[test]
fn memo_short_item_fits_one_page_with_header_block_and_footer() {
    let f = fonts();
    let pages = paginate(&PrintContent::Memo(vec![memo(3)]), &PageSetup::default(), &ctx(), &f);
    assert_eq!(pages.len(), 1);
    let t = texts(&pages[0]);
    for want in ["Pat Example", "From:", "Sent:", "To:", "Subject:", "Attachments:", "Quarterly numbers", "1"]
    {
        assert!(t.contains(&want), "missing {want:?} in {t:?}");
    }
    assert!(t.iter().any(|s| s.starts_with("Paragraph 2")));
    assert_eq!(pages[0].width, 816.0);
    assert_eq!(pages[0].height, 1056.0);
}

#[test]
fn memo_long_body_paginates_between_lines() {
    let f = fonts();
    let setup = PageSetup::default();
    let pages = paginate(&PrintContent::Memo(vec![memo(200)]), &setup, &ctx(), &f);
    assert!(pages.len() >= 4, "200 paragraphs need several pages, got {}", pages.len());
    let mut seen = std::collections::BTreeSet::new();
    for (i, p) in pages.iter().enumerate() {
        let t = texts(p);
        assert!(t.contains(&"Pat Example"), "name band on page {i}");
        assert!(t.contains(&(i + 1).to_string().as_str()), "page number {} in footer", i + 1);
        for s in t {
            if let Some(rest) = s.strip_prefix("Paragraph ") {
                let n: usize = rest.split(':').next().unwrap().parse().unwrap();
                assert!(seen.insert(n), "paragraph {n} printed twice");
            }
        }
        // Body text stays above the footer band.
        let limit = p.height - setup.margins.bottom;
        for item in &p.display.items {
            if let DisplayItem::Text { origin, .. } = item {
                assert!(origin.y <= limit + 1.0, "text below the bottom margin on page {i}");
            }
        }
    }
    assert_eq!(seen.len(), 200, "every paragraph printed exactly once");
}

#[test]
fn memo_items_start_on_new_pages_unless_disabled() {
    let f = fonts();
    let items = vec![memo(2), memo(2), memo(2)];
    let pages = paginate(&PrintContent::Memo(items.clone()), &PageSetup::default(), &ctx(), &f);
    assert_eq!(pages.len(), 3);
    let setup = PageSetup { new_page_per_item: false, ..PageSetup::default() };
    assert_eq!(paginate(&PrintContent::Memo(items), &setup, &ctx(), &f).len(), 1);
}

#[test]
fn landscape_a4_and_fields() {
    let f = fonts();
    let setup = PageSetup {
        paper: Paper::A4,
        orientation: Orientation::Landscape,
        header: ["[User Name]".into(), String::new(), "[Date Printed] [Time Printed]".into()],
        footer: [String::new(), "Page [Page #] of [Total Pages]".into(), String::new()],
        ..PageSetup::default()
    };
    let pages = paginate(&PrintContent::Memo(vec![memo(2), memo(2)]), &setup, &ctx(), &f);
    assert_eq!(pages.len(), 2);
    assert!(pages[0].width > pages[0].height);
    assert!((pages[0].width - 1122.5).abs() < 1.0);
    let t = texts(&pages[1]);
    assert!(t.contains(&"Page 2 of 2"), "{t:?}");
    assert!(t.contains(&"10/10/2026 9:00 AM"));
    assert_eq!(Paper::parse("a4"), Some(Paper::A4));
    assert_eq!(Paper::parse("tabloid"), None);
}

// ------------------------------------------------------------------ table

#[test]
fn table_repeats_header_on_every_page() {
    let f = fonts();
    let t = TableData {
        title: "Inbox".into(),
        columns: ["From", "Subject", "Received", "Size"]
            .iter()
            .zip([2.0, 4.0, 2.0, 1.0])
            .map(|(t, w)| TableColumn { title: (*t).into(), weight: w })
            .collect(),
        rows: (0..150)
            .map(|i| vec![format!("Sender {i}"), format!("Subject {i}"), "10/9/2026".into(), "3 KB".into()])
            .collect(),
        bold: vec![true; 3],
    };
    let pages = paginate(&PrintContent::Table(t), &PageSetup::default(), &ctx(), &f);
    assert!(pages.len() >= 3, "{}", pages.len());
    let mut rows = 0;
    for p in &pages {
        let tx = texts(p);
        assert!(tx.contains(&"Subject"), "header row on every page");
        rows += tx.iter().filter(|s| s.starts_with("Sender ")).count();
    }
    assert_eq!(rows, 150);
    assert!(texts(&pages[0]).contains(&"Inbox"));
}

// ------------------------------------------------------------------ calendar

fn cal(style: CalendarStyle, first: i64, last: i64) -> CalendarData {
    CalendarData {
        style,
        title: "Calendar".into(),
        first_day: first,
        last_day: last,
        week_start: 0,
        events: vec![
            CalendarEvent {
                start_day: first,
                start_min: 9 * 60,
                end_day: first,
                end_min: 10 * 60,
                subject: "Standup".into(),
                location: "Room 1".into(),
                ..Default::default()
            },
            CalendarEvent {
                start_day: first,
                start_min: 9 * 60 + 30,
                end_day: first,
                end_min: 11 * 60,
                subject: "Overlap".into(),
                ..Default::default()
            },
            CalendarEvent {
                start_day: first + 1,
                end_day: first + 2,
                all_day: true,
                subject: "Holiday".into(),
                ..Default::default()
            },
            CalendarEvent {
                start_day: first,
                start_min: 22 * 60,
                end_day: first,
                end_min: 23 * 60,
                subject: "Late".into(),
                ..Default::default()
            },
        ],
    }
}

#[test]
fn calendar_styles_page_counts_and_content() {
    let f = fonts();
    // 2026-10-12 is a Monday: day number 20738.
    let mon = mailcraft_model::dates::days_from_civil(2026, 10, 12).unwrap();
    let daily = paginate(
        &PrintContent::Calendar(cal(CalendarStyle::Daily, mon, mon + 2)),
        &PageSetup::default(),
        &ctx(),
        &f,
    );
    assert_eq!(daily.len(), 3);
    let t = texts(&daily[0]);
    assert!(t.contains(&"Monday, October 12, 2026"), "{t:?}");
    assert!(t.iter().any(|s| s.contains("Standup")));
    assert!(t.iter().any(|s| s.contains("Late")), "grid widens to late appointments");
    assert!(texts(&daily[1]).iter().any(|s| s.contains("Holiday")));
    assert!(!texts(&daily[2]).iter().any(|s| s.contains("Holiday")), "all-day ends at midnight");

    let weekly = paginate(
        &PrintContent::Calendar(cal(CalendarStyle::Weekly, mon, mon + 13)),
        &PageSetup::default(),
        &ctx(),
        &f,
    );
    assert_eq!(weekly.len(), 3, "Sunday-start weeks covering Oct 12–25");
    assert!(
        texts(&weekly[0]).iter().any(|s| s.contains("9:00 AM–10:00 AM") && s.contains("Standup (Room 1)"))
    );

    let monthly = paginate(
        &PrintContent::Calendar(cal(CalendarStyle::Monthly, mon, mon + 30)),
        &PageSetup::default(),
        &ctx(),
        &f,
    );
    assert_eq!(monthly.len(), 2, "October and November");
    let t = texts(&monthly[0]);
    assert!(t.contains(&"October 2026"));
    assert!(t.iter().any(|s| s.contains("Standup")));
}

// ------------------------------------------------------------------ contacts

fn contacts(n: usize) -> Vec<ContactCard> {
    (0..n)
        .map(|i| ContactCard {
            name: format!("{}, Person {i}", ["Adams", "Baker", "Clark"][i % 3]),
            fields: vec![
                ("Company".into(), "Example Ltd".into()),
                ("E-mail".into(), format!("p{i}@example.com")),
                ("Business".into(), "+1 555 0100".into()),
            ],
            phones: vec![("Business".into(), "+1 555 0100".into()), ("Mobile".into(), "+1 555 0101".into())],
        })
        .collect()
}

#[test]
fn contact_styles() {
    let f = fonts();
    for style in [ContactStyle::Card, ContactStyle::SmallBooklet, ContactStyle::PhoneDirectory] {
        let pages = paginate(
            &PrintContent::Contacts { style, cards: contacts(120) },
            &PageSetup::default(),
            &ctx(),
            &f,
        );
        assert!(pages.len() >= 2, "{style:?}: {}", pages.len());
        let names: usize =
            pages.iter().map(|p| texts(p).iter().filter(|s| s.contains("Person ")).count()).sum();
        assert_eq!(names, 120, "{style:?}: every contact printed once");
    }
    let dir = paginate(
        &PrintContent::Contacts { style: ContactStyle::PhoneDirectory, cards: contacts(3) },
        &PageSetup::default(),
        &ctx(),
        &f,
    );
    let t = texts(&dir[0]);
    assert!(t.contains(&"A") && t.contains(&"B") && t.contains(&"C"), "letter headings: {t:?}");
    assert!(t.contains(&"Mobile: +1 555 0101"));
}

// ------------------------------------------------------------------ PDF and preview

#[test]
fn pdf_is_structurally_valid_and_has_every_page() {
    let f = fonts();
    let pages = paginate(&PrintContent::Memo(vec![memo(200), memo(1)]), &PageSetup::default(), &ctx(), &f);
    let pdf = write_pdf(&pages, &f, &PdfInfo { title: "Print test".into(), author: "Pat".into() }).unwrap();
    let (objects, page_objs) = parse_pdf(&pdf);
    assert_eq!(page_objs, pages.len());
    assert!(objects > pages.len() * 2, "page + content objects at least");
    assert!(pdf.windows(9).any(|w| w == b"/FontFile"), "embedded font program");
    // Subsetting keeps the file small: well under one full bundled face per page.
    assert!(pdf.len() < 400_000, "{} bytes", pdf.len());
}

#[test]
fn pdf_of_every_style_and_of_hostile_items() {
    let f = fonts();
    let mon = mailcraft_model::dates::days_from_civil(2026, 10, 12).unwrap();
    let contents = vec![
        PrintContent::Calendar(cal(CalendarStyle::Monthly, mon, mon)),
        PrintContent::Contacts { style: ContactStyle::Card, cards: contacts(4) },
        PrintContent::Memo(Vec::new()),
    ];
    for c in contents {
        let pages = paginate(&c, &PageSetup::default(), &ctx(), &f);
        let pdf = write_pdf(&pages, &f, &PdfInfo::default()).unwrap();
        assert_eq!(parse_pdf(&pdf).1, pages.len());
    }
    let nan = f32::NAN;
    let hostile = Page {
        width: nan,
        height: -1.0,
        display: DisplayList {
            items: vec![
                DisplayItem::FillRect { rect: Rect::new(nan, 0.0, 1e30, 5.0), color: Color::BLACK },
                DisplayItem::StrokeRect {
                    rect: Rect::new(0.0, 0.0, 10.0, 10.0),
                    color: Color::BLACK,
                    width: f32::INFINITY,
                },
                DisplayItem::Line {
                    from: Point::new(0.0, 0.0),
                    to: Point::new(nan, 3.0),
                    color: Color::BLACK,
                    width: 1.0,
                },
                Style::new(&f, 10.0, false).text(10.0, 10.0, "\u{200B}\u{1F600}zero\u{200D}width 中文 \u{0}"),
                DisplayItem::Text {
                    origin: Point::new(10.0, 10.0),
                    text: "x".repeat(50_000),
                    font: mailcraft_fonts::FontId(9_999),
                    size_px: 1e9,
                    color: Color::BLACK,
                    underline: true,
                    strike: true,
                    link: None,
                },
                DisplayItem::Image {
                    rect: Rect::new(10.0, 10.0, 100.0, 50.0),
                    src: "cid:x".into(),
                    alt: "Logo".into(),
                },
            ],
        },
    };
    let pdf = write_pdf(&[hostile], &f, &PdfInfo::default()).unwrap();
    assert_eq!(parse_pdf(&pdf).1, 1);
}

#[test]
fn preview_renders_page_at_reduced_scale() {
    let f = fonts();
    let pages = paginate(&PrintContent::Memo(vec![memo(5)]), &PageSetup::default(), &ctx(), &f);
    let c = render_page(&pages[0], &f, 0.5);
    assert_eq!((c.width, c.height), (408, 528));
    let ink = (0..c.height)
        .flat_map(|y| (0..c.width).map(move |x| (x, y)))
        .filter(|&(x, y)| c.pixel(x, y) != Some(Color::WHITE))
        .count();
    assert!(ink > 500, "page has visible text and rules ({ink} px)");
    assert!(render_page(&pages[0], &f, f32::NAN).is_empty());
    assert!(render_page(&pages[0], &f, 0.0).is_empty());
}

#[test]
fn hostile_setup_and_content_do_not_panic() {
    let f = fonts();
    let setup = PageSetup {
        margins: Margins { top: f32::NAN, bottom: 1e9, left: -5.0, right: f32::INFINITY },
        header: ["[Page #]".repeat(1000), "\u{0}".into(), "x".repeat(100_000)],
        ..PageSetup::default()
    };
    let item = MemoItem {
        fields: vec![("\u{7}".repeat(10_000), "y".repeat(200_000)); 100],
        body: Doc::from_plain_text(&"word ".repeat(20_000)),
    };
    let pages = paginate(&PrintContent::Memo(vec![item]), &setup, &ctx(), &f);
    assert!(!pages.is_empty() && pages.len() <= MAX_PAGES);
    let empty = paginate(&PrintContent::Table(TableData::default()), &setup, &ctx(), &f);
    assert_eq!(empty.len(), 1);
    let cal = CalendarData { first_day: i64::MAX, last_day: i64::MIN, ..Default::default() };
    let _ = paginate(&PrintContent::Calendar(cal), &setup, &ctx(), &f);
    let cal =
        CalendarData { style: CalendarStyle::Daily, first_day: 0, last_day: i64::MAX, ..Default::default() };
    assert!(paginate(&PrintContent::Calendar(cal), &PageSetup::default(), &ctx(), &f).len() <= MAX_PAGES);
}

fn merge(style: MergeStyle, records: &[&str]) -> PrintContent {
    PrintContent::Merge(MergeDoc {
        style,
        records: records.iter().map(|r| (*r).to_owned()).collect(),
        return_address: "Pat Example\n1 Sender Way\nTown".into(),
    })
}

fn blank() -> PageSetup {
    PageSetup { footer: Default::default(), ..PageSetup::default() }
}

#[test]
fn form_letters_start_each_record_on_a_new_page() {
    let f = fonts();
    let long = format!("Dear Bo,\n\n{}", "A long letter line that wraps. ".repeat(400));
    let pages = paginate(&merge(MergeStyle::Letters, &["Dear Ann,\nHello.", &long]), &blank(), &ctx(), &f);
    assert!(pages.len() >= 3, "the second letter runs over more than one page: {}", pages.len());
    assert_eq!(texts(&pages[0]), ["Dear Ann,", "Hello."]);
    assert_eq!(texts(&pages[1]).first(), Some(&"Dear Bo,"));
    // An empty merge still makes one page.
    assert_eq!(paginate(&merge(MergeStyle::Letters, &[]), &blank(), &ctx(), &f).len(), 1);
}

#[test]
fn mailing_labels_fill_the_sheet_cells_in_order() {
    let f = fonts();
    let sheet = label_sheet("30").unwrap();
    assert_eq!(sheet.per_sheet(), 30);
    let recs: Vec<String> = (0..31).map(|i| format!("Person {i}\n{i} Main St\nTown")).collect();
    let refs: Vec<&str> = recs.iter().map(String::as_str).collect();
    let pages = paginate(&merge(MergeStyle::Labels(sheet), &refs), &blank(), &ctx(), &f);
    assert_eq!(pages.len(), 2, "31 labels need two sheets of 30");
    assert_eq!(texts(&pages[1]), ["Person 30", "30 Main St", "Town"]);
    // The second label sits in the second column, right of the first.
    let pos = |p: &Page, t: &str| {
        p.display.items.iter().find_map(|i| match i {
            DisplayItem::Text { text, origin, .. } if text == t => Some((origin.x, origin.y)),
            _ => None,
        })
    };
    let (x0, y0) = pos(&pages[0], "Person 0").unwrap();
    let (x1, y1) = pos(&pages[0], "Person 1").unwrap();
    let (x3, y3) = pos(&pages[0], "Person 3").unwrap();
    assert!(x1 > x0 + 200.0 && (y1 - y0).abs() < 0.5);
    assert!((x3 - x0).abs() < 0.5 && y3 > y0 + 80.0, "the fourth label starts the second row");
    // A label never prints outside its cell, even with many lines.
    let many = "line\n".repeat(40);
    let pages = paginate(&merge(MergeStyle::Labels(sheet), &[&many]), &blank(), &ctx(), &f);
    let cell = sheet.cell(0);
    for i in &pages[0].display.items {
        if let DisplayItem::Text { origin, .. } = i {
            assert!(origin.y > cell.y && origin.y < cell.y + cell.h, "{} outside the label", origin.y);
        }
    }
    assert!(label_sheet("nope").is_none());
    assert_eq!(label_sheet("21").unwrap().paper, Paper::A4);
}

#[test]
fn envelopes_put_one_address_per_envelope_with_the_return_address() {
    let f = fonts();
    let setup = PageSetup { paper: Paper::Envelope10, orientation: Orientation::Landscape, ..blank() };
    let recs = ["Ann Lee\n2 Elm Rd", "Bo Ek\n3 Oak Ave"];
    let pages = paginate(&merge(MergeStyle::Envelopes, &recs), &setup, &ctx(), &f);
    assert_eq!(pages.len(), 2);
    assert!((pages[0].width - 9.5 * 96.0).abs() < 0.5 && (pages[0].height - 4.125 * 96.0).abs() < 0.5);
    assert_eq!(texts(&pages[1]), ["Pat Example", "1 Sender Way", "Town", "Bo Ek", "3 Oak Ave"]);
    assert_eq!(Paper::parse("envelope dl"), Some(Paper::EnvelopeDl));
    let pdf = write_pdf(&pages, &f, &PdfInfo::default()).unwrap();
    assert!(pdf.starts_with(b"%PDF"));
}

#[test]
fn hostile_merge_records_do_not_panic() {
    let f = fonts();
    let junk = format!("{}\u{0}\u{202e}{}", "x".repeat(100_000), "\n".repeat(10_000));
    for style in [MergeStyle::Letters, MergeStyle::Labels(label_sheet("10").unwrap()), MergeStyle::Envelopes]
    {
        let pages = paginate(&merge(style, &[&junk, "", &junk]), &blank(), &ctx(), &f);
        assert!(!pages.is_empty() && pages.len() <= MAX_PAGES);
    }
    let odd = LabelSheet { cols: 0, rows: 0, w_in: -1.0, h_in: f32::NAN, ..label_sheet("30").unwrap() };
    let _ = paginate(&merge(MergeStyle::Labels(odd), &["a", "b"]), &blank(), &ctx(), &f);
}

/// Regression: a merge reports how many records its pages hold, so a cut-short document is never taken for
/// a complete one.
#[test]
fn merge_documents_report_the_records_they_hold() {
    let f = fonts();
    let doc = |style, n: usize, text: &str| MergeDoc {
        style,
        records: (0..n).map(|i| format!("{text} {i}")).collect(),
        return_address: String::new(),
    };
    let setup = PageSetup { paper: Paper::Envelope10, orientation: Orientation::Landscape, ..blank() };
    // Every record fits: all are counted.
    let (pages, placed) = paginate_merge(&doc(MergeStyle::Envelopes, 3, "Ann"), &setup, &ctx(), &f);
    assert_eq!((pages.len(), placed), (3, 3));
    // One envelope per page: the page limit holds MAX_PAGES of them.
    assert_eq!(max_records(&MergeStyle::Envelopes), MAX_PAGES);
    let (pages, placed) =
        paginate_merge(&doc(MergeStyle::Envelopes, MAX_PAGES + 5, "Ann"), &setup, &ctx(), &f);
    assert_eq!((pages.len(), placed), (MAX_PAGES, MAX_PAGES));
    // Labels: a sheet per page.
    let sheet = label_sheet("10").unwrap();
    assert_eq!(max_records(&MergeStyle::Labels(sheet)), 10 * MAX_PAGES);
    let (_, placed) = paginate_merge(&doc(MergeStyle::Labels(sheet), 25, "Bo"), &blank(), &ctx(), &f);
    assert_eq!(placed, 25);
    // Letters that run over several pages: a letter cut at the page limit does not count.
    let long = "line\n".repeat(4_000);
    let (pages, placed) = paginate_merge(&doc(MergeStyle::Letters, 40, &long), &blank(), &ctx(), &f);
    assert_eq!(pages.len(), MAX_PAGES);
    assert!(placed < 40 && placed > 0, "{placed}");
    let (_, placed) = paginate_merge(&doc(MergeStyle::Letters, 2, &long), &blank(), &ctx(), &f);
    assert_eq!(placed, 2);
}
