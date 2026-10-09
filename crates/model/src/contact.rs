use serde::{Deserialize, Serialize};

use crate::{FolderId, ItemId};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PostalAddress {
    /// "Business", "Home", "Other".
    pub kind: String,
    pub street: String,
    pub city: String,
    pub region: String,
    pub postal_code: String,
    pub country: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Contact {
    pub id: ItemId,
    pub folder: FolderId,
    pub first_name: String,
    pub middle_name: String,
    pub last_name: String,
    pub title: String,
    pub suffix: String,
    /// "File as" ("Last, First" by default).
    pub file_as: String,
    pub company: String,
    pub job_title: String,
    pub department: String,
    /// Up to three addresses (Email, Email 2, Email 3).
    pub emails: Vec<String>,
    /// (kind, number) e.g. ("Business", "+1 555 0100"), ("Mobile", …).
    pub phones: Vec<(String, String)>,
    pub addresses: Vec<PostalAddress>,
    pub web_page: String,
    pub im_address: String,
    /// "YYYY-MM-DD".
    pub birthday: Option<String>,
    pub notes: String,
    pub categories: Vec<String>,
}

impl Contact {
    /// "Title First Middle Last Suffix" with empty parts skipped.
    pub fn full_name(&self) -> String {
        [&self.title, &self.first_name, &self.middle_name, &self.last_name, &self.suffix]
            .iter()
            .filter(|s| !s.is_empty())
            .map(|s| s.as_str())
            .collect::<Vec<_>>()
            .join(" ")
    }
}
