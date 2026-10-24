//! Wall pages (B17 §3.2): the allow-list of web pages that the owner lets a
//! wall show in a closed frame, and the reference that a `page` directive
//! holds. A directive names an entry by id, never by URL.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// The most entries that a deck holds.
pub const MAX_WALL_PAGES: usize = 50;
/// The longest page name, in characters.
pub const MAX_PAGE_LABEL: usize = 60;

/// How the wall sizes the frame of a page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "display/")]
pub enum PageFit {
    /// The frame is one screen.
    Screen,
    /// The frame is 8 screens tall and the wall scrolls it.
    Long,
}

/// One entry of the allow-list, as the owner sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "display/")]
pub struct WallPage {
    /// A ULID.
    pub page_id: String,
    pub label: String,
    pub url: String,
    /// `scheme://host[:port]`, lower case, no default port.
    pub origin: String,
    pub fit: PageFit,
    /// Unix ms.
    pub created_at: i64,
}

/// The fields of a new entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "display/")]
pub struct NewWallPage {
    pub label: String,
    pub url: String,
    pub fit: PageFit,
}

/// `GET /api/wall-pages`: the entries, oldest first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "display/")]
pub struct WallPageList {
    pub pages: Vec<WallPage>,
}

/// A page in a directive: by id, never a URL.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "display/")]
pub struct PageRef {
    pub page_id: String,
}

/// `GET /api/displays/self/page/{page_id}`: what the wall needs to show it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "display/")]
pub struct DisplayPage {
    pub page_id: String,
    pub label: String,
    pub url: String,
    pub fit: PageFit,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wall_page_fit_uses_snake_case() {
        assert_eq!(
            serde_json::to_string(&PageFit::Screen).unwrap(),
            "\"screen\""
        );
        assert_eq!(serde_json::to_string(&PageFit::Long).unwrap(), "\"long\"");
        assert!(serde_json::from_str::<PageFit>("\"Long\"").is_err());
    }

    #[test]
    fn wall_page_new_entry_takes_only_its_own_fields() {
        let ok = r#"{"label":"Grafana","url":"https://g.example/d","fit":"long"}"#;
        let new: NewWallPage = serde_json::from_str(ok).unwrap();
        assert_eq!(new.fit, PageFit::Long);
        let extra = r#"{"label":"a","url":"https://a.example","fit":"screen","origin":"x"}"#;
        assert!(serde_json::from_str::<NewWallPage>(extra).is_err());
        let missing = r#"{"label":"a","url":"https://a.example"}"#;
        assert!(serde_json::from_str::<NewWallPage>(missing).is_err());
    }

    #[test]
    fn wall_page_ref_takes_only_the_id() {
        let r: PageRef =
            serde_json::from_str(r#"{"page_id":"01ARZ3NDEKTSV4RRFFQ69G5FAV"}"#).unwrap();
        assert_eq!(r.page_id.len(), 26);
        assert!(serde_json::from_str::<PageRef>(r#"{"page_id":"a","url":"b"}"#).is_err());
    }
}
