use mailcraft_model::Contact;
use proptest::prelude::*;

use crate::vcf::{self, Version};

#[test]
fn reads_vcard_4() {
    let text = "BEGIN:VCARD\r\nVERSION:4.0\r\nFN:Dr. Ann B. Lee Jr.\r\nN:Lee;Ann;B.;Dr.;Jr.\r\n\
ORG:Example\\, Inc.;Research\r\nTITLE:Director\r\nEMAIL;TYPE=work:ann@example.com\r\nEMAIL;PREF=1:first@example.com\r\n\
TEL;VALUE=uri;TYPE=\"work,voice\":tel:+1-555-0100\r\nTEL;TYPE=cell:+1 555 0101\r\n\
ADR;TYPE=home:;;1 Main St;Springfield;ON;K1A 0B1;Canada\r\nURL:https://example.com\r\nBDAY:19800229\r\n\
NOTE:Line one\\nLine two\\, with comma\r\nCATEGORIES:Clients,VIP\r\nEND:VCARD\r\n";
    let cs = vcf::read(text.as_bytes()).unwrap();
    assert_eq!(cs.len(), 1);
    let c = &cs[0];
    assert_eq!((c.title.as_str(), c.first_name.as_str(), c.middle_name.as_str()), ("Dr.", "Ann", "B."));
    assert_eq!((c.last_name.as_str(), c.suffix.as_str()), ("Lee", "Jr."));
    assert_eq!(c.company, "Example, Inc.");
    assert_eq!(c.department, "Research");
    assert_eq!(c.emails, vec!["first@example.com", "ann@example.com"]);
    assert_eq!(
        c.phones,
        vec![("Business".into(), "+1-555-0100".into()), ("Mobile".into(), "+1 555 0101".into())]
    );
    assert_eq!(c.addresses[0].kind, "Home");
    assert_eq!(c.addresses[0].postal_code, "K1A 0B1");
    assert_eq!(c.birthday.as_deref(), Some("1980-02-29"));
    assert_eq!(c.notes, "Line one\nLine two, with comma");
    assert_eq!(c.categories, vec!["Clients", "VIP"]);
    assert_eq!(c.file_as, "Lee, Ann");
}

#[test]
fn reads_vcard_3_and_2_1() {
    let text = "BEGIN:VCARD\nVERSION:3.0\nN:Stone;Bob;;;\nFN:Bob Stone\nitem1.EMAIL;type=INTERNET;type=pref:bob@example.org\n\
TEL;TYPE=HOME,FAX:111\nTEL;type=WORK:222\nBDAY:1990-01-31\nEND:VCARD\n\
BEGIN:VCARD\nVERSION:2.1\nN;CHARSET=ISO-8859-1;ENCODING=QUOTED-PRINTABLE:M=FCller;J=\n=FCrgen\nTEL;WORK;VOICE:333\n\
NOTE;ENCODING=QUOTED-PRINTABLE;CHARSET=UTF-8:caf=C3=A9\nEND:VCARD\n\
BEGIN:VCARD\nVERSION:4.0\nFN:Just A Name\nEND:VCARD\n";
    let cs = vcf::read(text.as_bytes()).unwrap();
    assert_eq!(cs.len(), 3);
    assert_eq!(cs[0].emails, vec!["bob@example.org"]);
    assert_eq!(cs[0].phones, vec![("Home Fax".into(), "111".into()), ("Business".into(), "222".into())]);
    assert_eq!(cs[0].birthday.as_deref(), Some("1990-01-31"));
    assert_eq!(cs[1].last_name, "M\u{fc}ller");
    assert_eq!(cs[1].first_name, "J\u{fc}rgen");
    assert_eq!(cs[1].phones, vec![("Business".into(), "333".into())]);
    assert_eq!(cs[1].notes, "caf\u{e9}");
    assert_eq!(
        (cs[2].first_name.as_str(), cs[2].middle_name.as_str(), cs[2].last_name.as_str()),
        ("Just", "A", "Name")
    );
}

#[test]
fn writes_and_reads_back_both_versions() {
    let mut c = super::csv::sample_contact();
    c.phones.push(("Assistant".into(), "+1 555 0109".into()));
    c.addresses.push(mailcraft_model::contact::PostalAddress {
        kind: "Other".into(),
        city: "Paris".into(),
        ..Default::default()
    });
    for v in [Version::V3, Version::V4] {
        let text = vcf::write(std::slice::from_ref(&c), v);
        for line in text.split("\r\n") {
            assert!(line.len() <= 75, "unfolded line: {line:?}");
        }
        assert!(text.starts_with("BEGIN:VCARD\r\nVERSION:"));
        let back = vcf::read(text.as_bytes()).unwrap();
        assert_eq!(back, vec![c.clone()], "version {v:?}:\n{text}");
    }
}

#[test]
fn custom_file_as_survives() {
    let c = Contact {
        first_name: "Ann".into(),
        last_name: "Lee".into(),
        file_as: "Annie".into(),
        ..Default::default()
    };
    let back = vcf::read(vcf::write(std::slice::from_ref(&c), Version::V4).as_bytes()).unwrap();
    assert_eq!(back[0].file_as, "Annie");
}

/// A 2 × 1 PNG (signature and IHDR are all the picture checks need).
fn png_2x1() -> Vec<u8> {
    vec![
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 13, b'I', b'H', b'D', b'R', 0, 0, 0, 2, 0,
        0, 0, 1, 8, 6, 0, 0, 0, 0xde, 0xad, 0xbe, 0xef,
    ]
}

/// A JPEG header with a 64 × 48 start-of-frame.
fn jpeg_64x48() -> Vec<u8> {
    let mut d = vec![0xff, 0xd8, 0xff, 0xe0, 0, 4, 0, 0, 0xff, 0xc0, 0, 11, 8, 0, 48, 0, 64, 3, 0, 0, 0];
    d.extend(std::iter::repeat_n(0x55, 600));
    d
}

fn detailed_contact() -> Contact {
    let mut c = super::csv::sample_contact();
    c.nickname = "Annie, A.".into();
    c.profession = "Engineer; civil".into();
    c.office = "Building 4, Room 210".into();
    c.manager = "Rivera, Alice".into();
    c.assistant = "Bob Stone".into();
    c.spouse = "Sam Lee".into();
    c.anniversary = Some("2010-06-19".into());
    c.picture = mailcraft_model::contact::ContactPicture::sniff(&jpeg_64x48());
    c
}

#[test]
fn details_fields_and_photo_round_trip() {
    let c = detailed_contact();
    assert!(c.picture.is_some());
    for v in [Version::V3, Version::V4] {
        let text = vcf::write(std::slice::from_ref(&c), v);
        for line in text.split("\r\n") {
            assert!(line.len() <= 75, "unfolded line: {line:?}");
        }
        let back = vcf::read(text.as_bytes()).unwrap();
        assert_eq!(back, vec![c.clone()], "version {v:?}:\n{text}");
    }
    let v4 = vcf::write(std::slice::from_ref(&c), Version::V4);
    assert!(v4.contains("\r\nPHOTO:data:image/jpeg;base64,/9j/"), "{v4}");
    assert!(v4.contains("\r\nNICKNAME:Annie\\, A.\r\n"), "{v4}");
    assert!(v4.contains("\r\nROLE:Engineer\\; civil\r\n"), "{v4}");
    assert!(v4.contains("\r\nANNIVERSARY:20100619\r\n"), "{v4}");
    assert!(v4.contains("\r\nRELATED;VALUE=text;TYPE=spouse:Sam Lee\r\n"), "{v4}");
    assert!(v4.contains("\r\nX-MAILCRAFT-MANAGER:Rivera\\, Alice\r\n"), "{v4}");
    let v3 = vcf::write(std::slice::from_ref(&c), Version::V3);
    assert!(v3.contains("\r\nPHOTO;ENCODING=b;TYPE=JPEG:/9j/"), "{v3}");
    assert!(v3.contains("\r\nX-ANNIVERSARY:2010-06-19\r\n"), "{v3}");
    assert!(v3.contains("\r\nX-MAILCRAFT-SPOUSE:Sam Lee\r\n"), "{v3}");
    // A PNG picture keeps its type.
    let mut p = c.clone();
    p.picture = mailcraft_model::contact::ContactPicture::sniff(&png_2x1());
    for v in [Version::V3, Version::V4] {
        let back = vcf::read_str(&vcf::write(std::slice::from_ref(&p), v));
        assert_eq!(back[0].picture, p.picture, "{v:?}");
    }
}

#[test]
fn reads_photos_in_every_encoding() {
    let b64 = mailcraft_mime::transfer::encode_base64_flat(&png_2x1());
    let jpeg = mailcraft_mime::transfer::encode_base64_flat(&jpeg_64x48());
    // vCard 2.1: bare TYPE, ENCODING=BASE64, value folded onto indented lines.
    let folded: String =
        jpeg.as_bytes().chunks(60).map(|c| format!("  {}\r\n", String::from_utf8_lossy(c))).collect();
    let text = format!(
        "BEGIN:VCARD\r\nVERSION:4.0\r\nFN:A\r\nPHOTO:data:image/png;base64,{b64}\r\nEND:VCARD\r\n\
BEGIN:VCARD\r\nVERSION:3.0\r\nFN:B\r\nPHOTO;ENCODING=b;TYPE=PNG:{b64}\r\nEND:VCARD\r\n\
BEGIN:VCARD\r\nVERSION:2.1\r\nFN:C\r\nPHOTO;ENCODING=BASE64;JPEG:\r\n{folded}\r\nEND:VCARD\r\n\
BEGIN:VCARD\r\nVERSION:4.0\r\nFN:D\r\nPHOTO:https://example.com/d.png\r\nEND:VCARD\r\n\
BEGIN:VCARD\r\nVERSION:4.0\r\nFN:E\r\nPHOTO:data:image/png;base64,bm90IGFuIGltYWdl\r\nEND:VCARD\r\n\
BEGIN:VCARD\r\nVERSION:4.0\r\nFN:F\r\nPHOTO:data:text/plain;base64,{b64}\r\nEND:VCARD\r\n"
    );
    let cs = vcf::read(text.as_bytes()).unwrap();
    assert_eq!(cs.len(), 6);
    let mime = |i: usize| cs[i].picture.as_ref().map(|p| p.mime.as_str());
    assert_eq!(mime(0), Some("image/png"));
    assert_eq!(cs[0].picture.as_ref().unwrap().data, png_2x1());
    assert_eq!(mime(1), Some("image/png"));
    assert_eq!(mime(2), Some("image/jpeg"));
    assert_eq!(cs[2].picture.as_ref().unwrap().data, jpeg_64x48());
    assert_eq!(mime(3), None, "remote photos are not fetched");
    assert_eq!(mime(4), None, "not a picture");
    assert_eq!(mime(5), None, "not an image type");
    // Too big a picture (by bytes or by pixels) is dropped.
    let big = mailcraft_mime::transfer::encode_base64_flat(&vec![
        0x89;
        mailcraft_model::contact::MAX_PICTURE_BYTES
            + 10
    ]);
    let mut wide = png_2x1();
    wide.splice(16..20, [0, 0, 0x20, 0]);
    let wide = mailcraft_mime::transfer::encode_base64_flat(&wide);
    for v in [big, wide] {
        let t = format!("BEGIN:VCARD\nFN:G\nPHOTO;ENCODING=b:{v}\nEND:VCARD\n");
        assert!(vcf::read_str(&t)[0].picture.is_none());
    }
}

#[test]
fn certificates_in_key_round_trip() {
    // A stand-in certificate: any DER SEQUENCE longer than 64 bytes is kept (the engine checks it).
    let mut der = vec![0x30, 0x81, 100];
    der.extend((0..100u8).map(|i| i.wrapping_mul(7)));
    let b64 = mailcraft_mime::transfer::encode_base64_flat(&der);
    for (version, key) in [
        ("4.0", format!("KEY:data:application/pkix-cert;base64,{b64}")),
        ("3.0", format!("KEY;ENCODING=b;TYPE=X509:{b64}")),
        ("2.1", format!("KEY;X509;ENCODING=BASE64:{b64}")),
    ] {
        let text = format!(
            "BEGIN:VCARD\r\nVERSION:{version}\r\nFN:Ann Lee\r\nEMAIL:ann@example.com\r\n{key}\r\nEND:VCARD\r\n"
        );
        let cs = vcf::read(text.as_bytes()).unwrap();
        assert_eq!(cs[0].certificates, vec![b64.clone()], "{version}");
        for v in [Version::V3, Version::V4] {
            let back = vcf::read(vcf::write(&cs, v).as_bytes()).unwrap();
            assert_eq!(back[0].certificates, vec![b64.clone()], "{version} written as {v:?}");
        }
    }
    // PGP keys, key URLs and junk are not certificates.
    for key in [
        "KEY;TYPE=PGP;ENCODING=b:AAAA",
        "KEY:https://example.com/key.asc",
        "KEY:data:application/pgp-keys;base64,AAAA",
        "KEY;ENCODING=b:!!!!",
    ] {
        let text = format!("BEGIN:VCARD\r\nVERSION:3.0\r\nFN:X\r\n{key}\r\nEND:VCARD\r\n");
        assert!(vcf::read(text.as_bytes()).unwrap()[0].certificates.is_empty(), "{key}");
    }
}

#[test]
fn reads_details_from_other_address_books() {
    let text = "BEGIN:VCARD\r\nVERSION:3.0\r\nFN:Ann Lee\r\nNICKNAME:Annie\r\nROLE:Surveyor\r\n\
X-MS-MANAGER:Max Power\r\nX-EVOLUTION-ASSISTANT:Ada\r\nX-KADDRESSBOOK-X-OFFICE:B4\r\n\
X-EVOLUTION-ANNIVERSARY:2001-02-03\r\nX-MS-SPOUSE:Sam\r\nEND:VCARD\r\n\
BEGIN:VCARD\r\nVERSION:3.0\r\nFN:Bo Ng\r\nitem1.X-ABRELATEDNAMES:Kim\r\nitem1.X-ABLabel:_$!<Spouse>!$_\r\n\
item2.X-ABRELATEDNAMES:Pat\r\nitem2.X-ABLabel:_$!<Manager>!$_\r\nitem3.X-ABDATE:1999-09-09\r\n\
item3.X-ABLabel:_$!<Anniversary>!$_\r\nitem4.X-ABRELATEDNAMES:Lou\r\nitem4.X-ABLabel:_$!<Friend>!$_\r\nEND:VCARD\r\n\
BEGIN:VCARD\r\nVERSION:4.0\r\nFN:Cy Ho\r\nRELATED;TYPE=spouse:urn:uuid:f81d4fae-7dec-11d0-a765-00a0c91e6bf6\r\n\
RELATED;TYPE=friend;VALUE=text:Jo\r\nANNIVERSARY:--0415\r\nEND:VCARD\r\n";
    let cs = vcf::read(text.as_bytes()).unwrap();
    let a = &cs[0];
    assert_eq!((a.nickname.as_str(), a.profession.as_str()), ("Annie", "Surveyor"));
    assert_eq!((a.manager.as_str(), a.assistant.as_str(), a.office.as_str()), ("Max Power", "Ada", "B4"));
    assert_eq!((a.spouse.as_str(), a.anniversary.as_deref()), ("Sam", Some("2001-02-03")));
    assert!(a.job_title.is_empty(), "ROLE is the profession, not the job title");
    let b = &cs[1];
    assert_eq!((b.spouse.as_str(), b.manager.as_str()), ("Kim", "Pat"));
    assert_eq!(b.anniversary.as_deref(), Some("1999-09-09"));
    assert!(b.assistant.is_empty());
    let c = &cs[2];
    assert!(c.spouse.is_empty(), "a URI relation is not a name");
    assert!(c.anniversary.is_none(), "a date without a year is dropped");
}

proptest! {
    #[test]
    fn vcf_never_panics(data in proptest::collection::vec(any::<u8>(), 0..1024)) {
        let _ = vcf::read(&data);
    }

    #[test]
    fn vcf_lines_never_panic(lines in proptest::collection::vec("[A-Z;=:,.\"\\\\a-z0-9 -]{0,40}", 0..12)) {
        let text = format!("BEGIN:VCARD\n{}\nEND:VCARD\n", lines.join("\n"));
        let _ = vcf::read_str(&text);
    }

    #[test]
    fn contact_round_trips(first in "[a-zA-Z\u{e9}\u{4e2d},;\\\\:]{1,12}", last in "[a-zA-Z ]{0,8}x", note in "[ -~\n\u{1F600}]{0,120}", company in "[a-zA-Z,; ]{0,10}") {
        let mut c = Contact {
            first_name: first,
            last_name: last.trim().to_owned(),
            notes: note,
            company: company.trim().to_owned(),
            emails: vec!["x@example.com".into()],
            ..Default::default()
        };
        c.file_as = crate::csv::default_file_as(&c);
        for v in [Version::V3, Version::V4] {
            let back = vcf::read_str(&vcf::write(std::slice::from_ref(&c), v));
            prop_assert_eq!(&back, &vec![c.clone()]);
        }
    }
}

#[test]
fn private_contacts_write_and_read_class() {
    let c =
        Contact { first_name: "Ann".into(), last_name: "Lee".into(), private: true, ..Default::default() };
    for v in [Version::V3, Version::V4] {
        let text = vcf::write(std::slice::from_ref(&c), v);
        assert!(text.contains("\r\nCLASS:PRIVATE\r\n"), "{text}");
        let back = vcf::read(text.as_bytes()).unwrap();
        assert!(back[0].private);
    }
    let public = Contact { first_name: "Bo".into(), ..Default::default() };
    assert!(!vcf::write(&[public], Version::V3).contains("CLASS"));
    let read = |class: &str| {
        let text = format!("BEGIN:VCARD\r\nVERSION:3.0\r\nFN:Cy\r\nCLASS:{class}\r\nEND:VCARD\r\n");
        vcf::read(text.as_bytes()).unwrap()[0].private
    };
    assert!(read("CONFIDENTIAL") && read("private"));
    assert!(!read("PUBLIC"));
}
