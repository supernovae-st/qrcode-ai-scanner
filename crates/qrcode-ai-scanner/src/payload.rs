//! Typed payload classification.
//!
//! Minimal in task A4 (the report contract needs the type); the full
//! classifier (url · wifi · email · sms · tel · geo · vcard · vevent)
//! lands in task A6 — variants are additive, the enum is non-exhaustive.

/// Classified payload of a decoded QR content string.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(tag = "kind", rename_all = "snake_case"))]
#[non_exhaustive]
pub enum Payload {
    /// Free text — the fallback class.
    Text,
}
