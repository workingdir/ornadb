//! Unicode 16.0.0 text primitives shared by OrnaDB layers.
//!
//! `casefold` implements `toNFKC_Casefold` for portable path ownership.
//! `case_properties` implements the Cased and Case_Ignorable properties used by
//! final-sigma detection.

pub mod case_properties;
pub mod casefold;
