#![forbid(unsafe_code)]

mod artifacts;
mod automation;
mod common;
mod descriptor;
mod envelope;
mod internal;
mod results;
mod tools;

pub use artifacts::*;
pub use automation::*;
pub use common::*;
pub use descriptor::*;
pub use envelope::*;
pub use internal::*;
pub use results::*;
pub use tools::*;

pub const PROTOCOL_VERSION: u32 = 1;
pub const STORE_SCHEMA_VERSION: u32 = 1;
pub const MAX_FRAME_BYTES: u64 = 262_144;
pub const SETTLEMENT_RESERVE_FLOOR: u64 = 16_384;
