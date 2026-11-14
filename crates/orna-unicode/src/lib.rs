//! Unicode 16.0.0 text primitives shared by OrnaDB layers.
//!
//! `casefold` implements `toNFKC_Casefold` for portable path ownership.
//! `case_properties` implements the Cased and Case_Ignorable properties used by
//! final-sigma detection. `simple_case` implements the White_Space property and the
//! simple case mappings with the Final_Sigma rule that the `core.text` natives use.

pub mod case_properties;
pub mod casefold;
pub mod simple_case;
