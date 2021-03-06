//! Keep track of time, both in native and web platforms!

#[cfg(target_arch = "wasm32")]
pub use instant::Instant;

#[cfg(not(target_arch = "wasm32"))]
pub use std::time::Instant;
pub use std::time::Duration;
