//! `error` owns the estate's typed-diagnostic derive.
//!
//! `std::error::Error` is a trait, and stable `std` has no derive that
//! generates `Display` or `source`. [`ParseError`](crate::ParseError) is built
//! with this derive, and code-observability consumers derive their diagnostics
//! from the same stack instead of each declaring `thiserror`.
//!
//! `#[derive(Error)]` expands to absolute `::thiserror::__private<N>::…` paths
//! that the *consuming* crate resolves, so a consumer names this crate
//! `thiserror` at its root:
//!
//! ```
//! extern crate lgwks_ast as thiserror;
//!
//! #[derive(thiserror::Error, Debug)]
//! enum Diagnostic {
//!     #[error("parse refused: {0}")]
//!     Refused(String),
//!     #[error(transparent)]
//!     Io(#[from] std::io::Error),
//! }
//! ```
//!
//! The crate root carries `pub use thiserror::*;`, which is what re-exports the
//! version-suffixed `__private<N>` module the expansion needs.

pub use thiserror::Error;
