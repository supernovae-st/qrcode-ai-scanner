//! Symbology substrate — the matrix-level primitives every symbology proof is
//! built ON, independent of scoring.
//!
//! These are the pieces a QR symbol is MADE of, not the pieces that JUDGE it:
//! the GF(256) field ([`gf256`]), the version database ([`version_db`]), the
//! zigzag module-placement walk ([`zigzag`]), the Reed-Solomon block
//! de-interleave ([`deinterleave`]), and the perspective grid sampler
//! ([`sampler`]). Both the scoring stage (`score::uec`, `score::structural`,
//! `score::iso15415`) and the rescue stage (`rescue`) build on this layer —
//! keeping it here is what lets the truth chain (rescue) stay independent of
//! the scoring module.
//!
//! QD-6 insurance: owning the matrix substrate means a dying engine dependency
//! only costs position detection, never the decode / rescue / margin chain.

pub(crate) mod deinterleave;
pub(crate) mod gf256;
pub(crate) mod sampler;
pub(crate) mod version_db;
pub(crate) mod zigzag;
