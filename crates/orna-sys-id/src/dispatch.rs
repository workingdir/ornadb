//! Generated exhaustive dispatch over the catalogued callable set.
//!
//! [`SysFunctionHost`] declares one required method per `SysFunction` variant and
//! [`dispatch`] calls the matching method from a `match` with no wildcard arm. A
//! new catalogue callable therefore stops every host implementation compiling
//! until the identity is explicitly classified as implemented or unavailable.

include!(concat!(env!("OUT_DIR"), "/dispatch.rs"));
