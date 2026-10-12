//! The families as tests: one `#[test]` per band of each family, the
//! coverage checks over one band, and the replay and divergence checks over
//! every family at once.

use crate::script::lexicon::{Kind, LEXICON};

use super::labels::label_family;
use super::passthrough::passthrough_family;
use super::{
    PLANTS, SCRIPT_BASE, SEEDS_PER_BAND, Script, expansion_family, misalignment_family,
    nested_plant_family, plant_family, planting, tree_family,
};

/// One `#[test]` per band of a family, so a failing band names itself.
macro_rules! bands {
    ($family:ident: $($test:ident = $band:literal),+ $(,)?) => {
        $(
            #[test]
            fn $test() -> Result<(), String> {
                $family(SEEDS_PER_BAND.saturating_mul($band), SEEDS_PER_BAND).map(drop)
            }
        )+
    };
}

bands!(tree_family:
    sim_the_line_tree_matches_the_indentation_model_band_00 = 0,
    sim_the_line_tree_matches_the_indentation_model_band_01 = 1,
    sim_the_line_tree_matches_the_indentation_model_band_02 = 2,
    sim_the_line_tree_matches_the_indentation_model_band_03 = 3,
);

bands!(misalignment_family:
    sim_a_misaligned_line_is_refused_at_its_own_line_band_00 = 0,
    sim_a_misaligned_line_is_refused_at_its_own_line_band_01 = 1,
);

bands!(expansion_family:
    sim_every_generated_script_expands_band_00 = 0,
    sim_every_generated_script_expands_band_01 = 1,
);

bands!(plant_family:
    sim_a_planted_refusal_is_spanned_at_its_own_token_band_00 = 0,
    sim_a_planted_refusal_is_spanned_at_its_own_token_band_01 = 1,
    sim_a_planted_refusal_is_spanned_at_its_own_token_band_02 = 2,
    sim_a_planted_refusal_is_spanned_at_its_own_token_band_03 = 3,
);

bands!(nested_plant_family:
    sim_a_refusal_nested_in_brackets_is_still_refused_band_00 = 0,
    sim_a_refusal_nested_in_brackets_is_still_refused_band_01 = 1,
);

bands!(label_family:
    sim_step_labels_survive_a_line_inserted_above_band_00 = 0,
    sim_step_labels_survive_a_line_inserted_above_band_01 = 1,
);

bands!(passthrough_family:
    sim_the_map_lists_exactly_the_plain_rust_lines_band_00 = 0,
    sim_the_map_lists_exactly_the_plain_rust_lines_band_01 = 1,
);

#[test]
fn sim_one_band_of_scripts_says_every_word() -> Result<(), String> {
    let mut said: Vec<Kind> = Vec::new();
    for offset in 0..SEEDS_PER_BAND {
        let script = Script::drawn(SCRIPT_BASE.wrapping_add(offset))?;
        for kind in script.said {
            if !said.contains(&kind) {
                said.push(kind);
            }
        }
    }
    let unsaid: Vec<&str> = LEXICON
        .iter()
        .filter(|row| !said.contains(&row.kind()))
        .map(|row| row.kind().spelling())
        .collect();
    if unsaid.is_empty() {
        Ok(())
    } else {
        Err(format!("no script in the band says {unsaid:?}"))
    }
}

#[test]
fn sim_one_band_plants_every_refusal() -> Result<(), String> {
    let (_, drawn) = planting(0, SEEDS_PER_BAND, false)?;
    let missing: Vec<&str> = PLANTS
        .iter()
        .enumerate()
        .filter(|&(index, _)| !drawn.contains(&index))
        .map(|(_, plant)| plant.line)
        .collect();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(format!("the band never planted {missing:?}"))
    }
}

/// Every family, over `count` seeds from `first`, as one trace.
fn every_family(first: u64, count: u64) -> Result<Vec<u64>, String> {
    Ok(vec![
        tree_family(first, count)?,
        misalignment_family(first, count)?,
        expansion_family(first, count)?,
        plant_family(first, count)?,
        nested_plant_family(first, count)?,
        label_family(first, count)?,
        passthrough_family(first, count)?,
    ])
}

#[test]
fn sim_the_same_seed_replays_to_the_same_trace() -> Result<(), String> {
    let once = every_family(0, 64)?;
    let again = every_family(0, 64)?;
    if once == again {
        Ok(())
    } else {
        Err(format!(
            "one seed range traced {once:x?} and then {again:x?}"
        ))
    }
}

#[test]
fn sim_distinct_seeds_diverge_in_every_family() -> Result<(), String> {
    let low = every_family(0, 64)?;
    let high = every_family(64, 64)?;
    let same: Vec<usize> = low
        .iter()
        .zip(&high)
        .enumerate()
        .filter(|&(_, (left, right))| left == right)
        .map(|(family, _)| family)
        .collect();
    if same.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "families {same:?} traced two seed ranges identically"
        ))
    }
}

#[test]
fn sim_four_thousand_seeds_hold_every_property() -> Result<(), String> {
    every_family(1_048_576, 4096).map(drop)
}
