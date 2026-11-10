//! Generated production provider linkage for the portable `sys` surface.
//!
//! One entry per catalogue member in catalogue order. `None` is an explicit
//! "catalogued but no production host binds it" classification, so a newly
//! catalogued member appears here as a compile-visible row rather than being
//! silently absent.

include!(concat!(env!("OUT_DIR"), "/provider_links.rs"));
