use mailcraft_model::Contact;
use mailcraft_model::contact::PostalAddress;
use proptest::prelude::*;

use crate::csv;

#[test]
fn rfc4180_parsing() {
    let rows = csv::parse("a,b,c\r\n\"x, y\",\"he said \"\"hi\"\"\",\"line1\nline2\"\r\n\r\nlast,,\n", ',');
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0], vec!["a", "b", "c"]);
    assert_eq!(rows[1], vec!["x, y", "he said \"hi\"", "line1\nline2"]);
    assert_eq!(rows[2], vec!["last", "", ""]);
    // No trailing newline; a quoted empty field still counts.
    assert_eq!(csv::parse("a,\"\"", ','), vec![vec!["a".to_owned(), String::new()]]);
    assert_eq!(csv::parse("", ','), Vec::<Vec<String>>::new());
}

#[test]
fn delimiter_detection() {
    assert_eq!(csv::detect_delimiter("First;Last;Email\r\n"), ';');
    assert_eq!(csv::detect_delimiter("First\tLast\n"), '\t');
    assert_eq!(csv::detect_delimiter("\"a;b\",c\n"), ',');
    assert_eq!(csv::detect_delimiter("single"), ',');
    let rows = csv::parse_bytes(b"\xEF\xBB\xBFName;Mail\r\nAnn;ann@example.com\r\n").unwrap();
    assert_eq!(rows[1], vec!["Ann", "ann@example.com"]);
    // Windows-1252 fallback.
    let rows = csv::parse_bytes(b"Name\r\nZo\xEB\r\n").unwrap();
    assert_eq!(rows[1], vec!["Zo\u{eb}"]);
}

#[test]
fn writer_quotes_only_when_needed() {
    let out = csv::write(&[vec!["plain", "a,b", "q\"t", "multi\nline", " pad"]], ',');
    assert_eq!(out, "plain,\"a,b\",\"q\"\"t\",\"multi\nline\",\" pad\"\r\n");
}

#[test]
fn export_columns_map_back() {
    assert!(csv::columns_consistent());
}

pub(crate) fn sample_contact() -> Contact {
    Contact {
        first_name: "Ann".into(),
        middle_name: "B.".into(),
        last_name: "Lee".into(),
        title: "Dr.".into(),
        suffix: "Jr.".into(),
        file_as: "Lee, Ann".into(),
        company: "Example, Inc.".into(),
        department: "Research".into(),
        job_title: "Director".into(),
        emails: vec!["ann@example.com".into(), "ann.lee@example.org".into()],
        phones: vec![
            ("Business".into(), "+1 555 0100".into()),
            ("Home Fax".into(), "+1 555 0102".into()),
            ("Mobile".into(), "+1 555 0101".into()),
        ],
        addresses: vec![PostalAddress {
            kind: "Business".into(),
            street: "1 Main St\nSuite 4".into(),
            city: "Springfield".into(),
            region: "ON".into(),
            postal_code: "K1A 0B1".into(),
            country: "Canada".into(),
        }],
        web_page: "https://example.com".into(),
        im_address: "ann@chat.example.com".into(),
        birthday: Some("1980-02-29".into()),
        notes: "Met at the \"expo\";\nlikes tea, coffee".into(),
        categories: vec!["Clients".into(), "VIP".into()],
        ..Default::default()
    }
}

#[test]
fn contacts_round_trip() {
    let c = sample_contact();
    let text = csv::export_contacts(std::slice::from_ref(&c));
    assert!(text.starts_with("First Name,Middle Name,Last Name,"));
    let back = csv::import_contacts(text.as_bytes()).unwrap();
    assert!(back.unmapped_columns.is_empty());
    assert_eq!(back.contacts, vec![c]);
}

#[test]
fn imports_common_address_book_headers() {
    let data = "Name,Given Name,Family Name,E-mail 1 - Value,E-mail 2 - Value,Phone 1 - Type,Phone 1 - Value,\
Phone 2 - Type,Phone 2 - Value,Organization 1 - Name,Organization 1 - Title,Address 1 - Type,Address 1 - City,\
Birthday,Group Membership,Mystery\r\n\
Bob Stone,Bob,Stone,bob@example.org,b@example.net,Mobile,555-0199,Work Fax,555-0198,Acme,Engineer,Home,Ottawa,\
1/31/1990,* myContacts ::: Friends,?\r\n";
    let r = csv::import_contacts(data.as_bytes()).unwrap();
    assert_eq!(r.unmapped_columns, vec!["Mystery".to_owned()]);
    let c = &r.contacts[0];
    assert_eq!((c.first_name.as_str(), c.last_name.as_str()), ("Bob", "Stone"));
    assert_eq!(c.emails, vec!["bob@example.org", "b@example.net"]);
    assert_eq!(
        c.phones,
        vec![("Mobile".into(), "555-0199".into()), ("Business Fax".into(), "555-0198".into())]
    );
    assert_eq!(c.company, "Acme");
    assert_eq!(c.job_title, "Engineer");
    assert_eq!(c.addresses[0].kind, "Home");
    assert_eq!(c.addresses[0].city, "Ottawa");
    assert_eq!(c.birthday.as_deref(), Some("1990-01-31"));
    assert_eq!(c.categories, vec!["myContacts", "Friends"]);
    assert_eq!(c.file_as, "Stone, Bob");
}

#[test]
fn full_name_only_and_semicolons() {
    let r =
        csv::import_contacts(b"Display Name;E-mail Address;Work Phone\nCy Mae Dunn;cy@example.net;123\n;;\n")
            .unwrap();
    assert_eq!(r.contacts.len(), 1);
    let c = &r.contacts[0];
    assert_eq!((c.first_name.as_str(), c.middle_name.as_str(), c.last_name.as_str()), ("Cy", "Mae", "Dunn"));
    assert_eq!(c.phones, vec![("Business".into(), "123".into())]);
    // Unparseable birthdays are dropped, not kept as garbage.
    let r = csv::import_contacts(b"First Name,Birthday\nA,0/0/00\n").unwrap();
    assert_eq!(r.contacts[0].birthday, None);
}

proptest! {
    #[test]
    fn csv_round_trips(rows in proptest::collection::vec(proptest::collection::vec("[ -~\n\u{e9}\u{4e2d}]{0,12}", 1..5), 1..6)) {
        // Rows of one empty field are blank lines (skipped by design); keep at least one non-empty field.
        let rows: Vec<Vec<String>> = rows.into_iter().filter(|r| !(r.len() == 1 && r[0].is_empty())).collect();
        let text = csv::write(&rows, ',');
        prop_assert_eq!(csv::parse(&text, ','), rows);
    }

    #[test]
    fn csv_never_panics(data in proptest::collection::vec(any::<u8>(), 0..1024)) {
        let _ = csv::parse_bytes(&data);
        let _ = csv::import_contacts(&data);
    }

    #[test]
    fn contact_fields_round_trip(first in "[a-zA-Z ,;\"]{0,10}", last in "[a-zA-Z]{1,10}", note in "[ -~\n]{0,30}", email in "[a-z]{1,8}@example\\.com") {
        let c = Contact {
            first_name: first.trim().to_owned(),
            last_name: last.clone(),
            emails: vec![email],
            notes: note.trim_matches(|ch: char| ch.is_whitespace()).to_owned(),
            ..Default::default()
        };
        let mut c = c;
        c.file_as = csv::default_file_as(&c);
        let text = csv::export_contacts(std::slice::from_ref(&c));
        let back = csv::import_contacts(text.as_bytes()).unwrap();
        prop_assert_eq!(back.contacts, vec![c]);
    }
}
