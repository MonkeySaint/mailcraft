use std::path::Path;

use crate::{Format, FormatError, check_size};

#[test]
fn format_from_extension() {
    assert_eq!(Format::from_path(Path::new("a/b/Message.MSG")), Some(Format::Msg));
    assert_eq!(Format::from_path(Path::new("x.ics")), Some(Format::Ics));
    assert_eq!(Format::from_path(Path::new("x.vcf")), Some(Format::Vcf));
    assert_eq!(Format::from_path(Path::new("x.csv")), Some(Format::Csv));
    assert_eq!(Format::from_path(Path::new("x.eml")), Some(Format::Eml));
    assert_eq!(Format::from_path(Path::new("Inbox.mbox")), Some(Format::Mbox));
    assert_eq!(Format::from_path(Path::new("x.txt")), None);
    assert_eq!(Format::from_path(Path::new("noext")), None);
    assert_eq!(Format::Ics.mime_type(), "text/calendar");
    assert_eq!(Format::Vcf.extension(), "vcf");
}

#[test]
fn format_from_content() {
    assert_eq!(Format::sniff(&crate::msg::CFB_SIGNATURE), Some(Format::Msg));
    assert_eq!(Format::sniff(b"\xEF\xBB\xBFBEGIN:VCALENDAR\r\n"), Some(Format::Ics));
    assert_eq!(Format::sniff(b"  begin:vcard\n"), Some(Format::Vcf));
    assert_eq!(Format::sniff(b"From a@example.com Thu Oct  8 04:30:00 2026\n"), Some(Format::Mbox));
    assert_eq!(Format::sniff(b"Subject: hi\r\n\r\nbody"), Some(Format::Eml));
    assert_eq!(Format::sniff(b"First Name,Last Name\r\n"), Some(Format::Csv));
    assert_eq!(Format::sniff(b""), None);
}

#[test]
fn size_cap() {
    assert!(check_size("x", 10, 10).is_ok());
    assert!(matches!(check_size("x", 11, 10), Err(FormatError::TooLarge { size: 11, limit: 10, .. })));
}

proptest::proptest! {
    #[test]
    fn sniff_never_panics(data in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..512)) {
        let _ = Format::sniff(&data);
    }
}
