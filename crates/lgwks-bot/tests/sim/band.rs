//! The seed bands every simulation family sweeps.
//!
//! A file of its own so a family that needs only the bands and the generator —
//! `sim_review_path`, its own binary for the saturation lane — includes exactly
//! that, and not the journal rig and network the rest of the substrate carries.

/// A seed band: the seeds one declared test sweeps.
///
/// A band rather than a single seed so that a test is a *sweep* and not a
/// point sample, and a band rather than every seed so that a failure names the
/// band and the offending seed is recovered from the recorded trace. The union
/// of the bands is a contiguous sweep of the whole space, so no seed is
/// quietly untested because of how the bands were cut.
#[derive(Clone, Copy, Debug)]
pub struct Band {
    /// The first seed, inclusive.
    pub first: u64,
    /// How many seeds the band covers.
    pub count: u64,
}

impl Band {
    /// A band of `count` seeds starting at `first`.
    pub const fn new(first: u64, count: u64) -> Self {
        Self { first, count }
    }

    /// Every seed in the band.
    pub fn seeds(&self) -> impl Iterator<Item = u64> + '_ {
        self.first..self.first.saturating_add(self.count)
    }
}

/// The seed space every family sweeps, and how many bands it is cut into.
///
/// One pair for the whole simulation layer. Each file that declared its own
/// would be free to drift, and a family sweeping 64 seeds next to one sweeping
/// 128 would make a pass rate that means nothing. The seed space is doubled
/// alongside the band count, so every band holds eight seeds rather than eight
/// families sharing one — a family registered against band 30 would otherwise
/// silently sweep the same eight seeds as band 14.
pub const SEED_SPACE: u64 = 256;
/// How many bands [`SEED_SPACE`] is cut into.
pub const BANDS: u64 = 32;

/// The one band of seeds a declared test sweeps.
///
/// `index` is a registration, not a computation: every call site is a literal
/// written beside the family that declares it. An index inside the table is
/// that band; an index past it is the next band-width of seeds past
/// [`SEED_SPACE`], one band per index. Folding indices past the table onto its
/// last band made families 32 to 44 sweep the same eight seeds (#401), and a
/// table widened for them would double every sweep that walks the whole table.
pub fn band_of(index: usize) -> Band {
    let table = bands(SEED_SPACE, BANDS);
    if let Some(band) = table.get(index) {
        return *band;
    }
    let (Some(last), Ok(beyond)) = (table.last(), u64::try_from(index.saturating_sub(table.len())))
    else {
        return Band::new(0, 1);
    };
    Band::new(
        SEED_SPACE.saturating_add(beyond.saturating_mul(last.count)),
        last.count,
    )
}

/// The bands that partition `0..total` into `parts` of them.
///
/// Even division with the remainder spread over the low bands, so the bands
/// are contiguous, disjoint, and together cover every seed with no gap and no
/// overlap. A suite that leaves a hole in the seed space would be reporting a
/// coverage it does not have. Zero parts is no bands.
pub fn bands(total: u64, parts: u64) -> Vec<Band> {
    let (Some(base), Some(remainder)) = (total.checked_div(parts), total.checked_rem(parts)) else {
        return Vec::new();
    };
    (0..parts)
        .scan(0u64, |cursor, index| {
            let count = base.saturating_add(u64::from(index < remainder));
            let band = Band::new(*cursor, count);
            *cursor = cursor.saturating_add(count);
            Some(band)
        })
        .collect()
}
