//! The activity object sent with `SET_ACTIVITY`.
//!
//! Field names follow Discord's activity object. Only the fields DeskPresence
//! uses are modelled; absent fields are omitted from the JSON.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct Activity {
    /// 0 Playing, 2 Listening, 3 Watching, 5 Competing (the types permitted
    /// for `SET_ACTIVITY`).
    #[serde(rename = "type")]
    pub activity_type: u8,
    /// Experimental activity-name override (see `DiscordConfig`).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub details: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub state: Option<String>,
    /// Which field the member-list status shows (1 state, 2 details).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub status_display_type: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub timestamps: Option<ActivityTimestamps>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub assets: Option<ActivityAssets>,
}

/// Start time in Unix **seconds**, matching the `SET_ACTIVITY` example in
/// Discord's RPC documentation (`time(nullptr)`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct ActivityTimestamps {
    #[serde(skip_serializing_if = "Option::is_none", default)]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub start: Option<u64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct ActivityAssets {
    #[serde(skip_serializing_if = "Option::is_none", default)]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub large_image: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub large_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub small_image: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub small_text: Option<String>,
}

impl ActivityAssets {
    pub fn is_empty(&self) -> bool {
        self.large_image.is_none()
            && self.large_text.is_none()
            && self.small_image.is_none()
            && self.small_text.is_none()
    }
}

impl Activity {
    pub fn has_assets(&self) -> bool {
        self.assets.as_ref().is_some_and(|a| !a.is_empty())
    }

    /// Copy without artwork, used when Discord rejects the assets so the rest
    /// of the presence can still be shown.
    pub fn without_assets(&self) -> Activity {
        Activity {
            assets: None,
            ..self.clone()
        }
    }
}
