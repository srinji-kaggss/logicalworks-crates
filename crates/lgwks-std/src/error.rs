//! `error` owns the estate's typed-error derive.
//!
//! `std::error::Error` is a trait, and stable `std` has no derive that
//! generates `Display` or `source`. `thiserror` is the one honest oracle for
//! that expansion, so it is a `lgwks_std` boundary rather than a consumer-local
//! edge: the register carries `owner = "lgwks_std"`, and a downstream crate
//! never declares `thiserror` itself.
//!
//! `#[derive(Error)]` expands to absolute `::thiserror::__private<N>::…` paths
//! that the *consuming* crate resolves, so a consumer names this crate
//! `thiserror` at its root. The root `pub use thiserror::*;` in the crate root
//! is what carries the version-suffixed private module there.
//!
//! ```
//! extern crate lgwks_std as thiserror;
//!
//! #[derive(thiserror::Error, Debug)]
//! enum StoreError {
//!     #[error("value length {actual} exceeds maximum {maximum}")]
//!     Length { actual: usize, maximum: usize },
//!     #[error(transparent)]
//!     Io(#[from] std::io::Error),
//! }
//! ```
//!
//! The feature costs two crates over `json`: `thiserror` and the
//! `thiserror-impl` derive reuse the `proc-macro2`, `quote`, and `syn` stack
//! `serde`'s derive already builds.

pub use thiserror::Error;
