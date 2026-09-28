//! The one band-declaration macro for the simulation layer.
//!
//! A file rather than an item in `mod.rs`, because a macro invoked only by the
//! four families that declare sweeps reads as unused in the test crates that do
//! not, and an `expect` that fires in one crate and not another is a lint that
//! fails the build in both directions. Only the families that sweep include it.

/// Declare one test per band for a family.
///
/// One definition for the whole layer. A copy per file is how two families end
/// up with different band arithmetic and one quietly sweeps nothing.
macro_rules! band_family {
    ($($name:ident => $family:path, $index:expr);+ $(;)?) => {
        $(
            /// A seeded sweep of this family's property.
            #[test]
            fn $name() -> TestResult {
                $family(sim::band_of($index))
            }
        )+
    };
}

pub(crate) use band_family;
