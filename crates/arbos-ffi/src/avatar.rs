//! Avatar colours for Swift: the D79 index rule and the sender accent bar
//! (`arbos_core::avatar`), so the iPhone draws the same hex as the Mac.

use arbos_core::avatar;

/// A sender accent in both appearances, as `#RRGGBB`.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct AccentHex {
    pub dark: String,
    pub light: String,
}

impl From<avatar::Accent> for AccentHex {
    fn from(a: avatar::Accent) -> Self {
        Self {
            dark: a.dark.hex(),
            light: a.light.hex(),
        }
    }
}

/// D79: the gradient index for a stable id, or the owner's override (0–6).
#[uniffi::export]
pub fn avatar_color_index(seed: String, override_index: Option<u8>) -> u8 {
    avatar::index_for(&seed, override_index)
}

/// The dominant vibrant colour of a decoded picture (`width × height` RGBA,
/// any size), or `None` when it is colourless and the D79 fallback applies.
#[uniffi::export]
pub fn avatar_dominant_accent(rgba: Vec<u8>, width: u32, height: u32) -> Option<AccentHex> {
    avatar::dominant_accent(&rgba, width, height).map(Into::into)
}

/// The D79 fallback accent: the midpoint of the sender's gradient.
#[uniffi::export]
pub fn avatar_fallback_accent(seed: String, override_index: Option<u8>) -> AccentHex {
    avatar::fallback_accent(&seed, override_index).into()
}

/// Bump of the extraction rules; part of any persisted cache key.
#[uniffi::export]
pub fn avatar_accent_version() -> u32 {
    avatar::ACCENT_VERSION
}
