//! TURN issuance authorized by a Lite/Full ring-VRF membership proof instead
//! of an app-integrity JWT.
//!
//! Verification authority comes only from server configuration and cached
//! chain state. The recovered alias is used for in-memory throttling and
//! keyed credential-id derivation; it must not appear in responses, logs, or metrics.

pub mod context;
pub mod message;
pub mod roots;
pub mod verify;
