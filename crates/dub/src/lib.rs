//! Process wiring for Device Uniqueness Backend — see `src/main.rs` for the binary.
//!
//! The library half exists so dev-only tooling can read what the binary knows
//! without shelling out to it: `crates/routegen` renders the committed edge
//! configs from [`routes::table`], the same way `apidoc-gen` renders the
//! committed API reference from the services' annotations.

pub mod roles;
pub mod routes;
pub mod shutdown;
