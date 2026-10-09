//! Unit tests for the model crate.

mod category;
mod dates;
mod item;
mod mailbox;
mod rich;
mod rule;
mod subject;
mod text;
mod thread;

use crate::{Address, DateTime, MailItem};

/// Local offset used by the date tests (EDT, -04:00).
pub const EDT: i32 = -240;

/// Local civil time in EDT.
pub fn edt(y: i64, mo: u8, d: u8, h: u8, mi: u8) -> DateTime {
    crate::dates::local_datetime(y, mo, d, h, mi, 0, EDT).expect("valid test date")
}

pub fn addr(name: &str, a: &str) -> Address {
    Address::new(if name.is_empty() { None } else { Some(name) }, a)
}

pub fn mail(subject: &str) -> MailItem {
    MailItem { subject: subject.to_string(), ..Default::default() }
}
