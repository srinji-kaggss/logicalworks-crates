//! Further seed bands of the lane-environment simulation: bands 3 to 18 of
//! the four properties `sim_env` checks, each over 64 seeds.

use super::{
    backtrace_is_kept_or_one, colour_is_never, incremental_defaults_to_zero, incremental_is_kept,
    over_band,
};

#[test]
fn sim_incremental_is_kept_band_3() {
    over_band(3, incremental_is_kept);
}

#[test]
fn sim_incremental_defaults_to_zero_band_3() {
    over_band(3, incremental_defaults_to_zero);
}

#[test]
fn sim_colour_is_never_band_3() {
    over_band(3, colour_is_never);
}

#[test]
fn sim_backtrace_is_kept_or_one_band_3() {
    over_band(3, backtrace_is_kept_or_one);
}

#[test]
fn sim_incremental_is_kept_band_4() {
    over_band(4, incremental_is_kept);
}

#[test]
fn sim_incremental_defaults_to_zero_band_4() {
    over_band(4, incremental_defaults_to_zero);
}

#[test]
fn sim_colour_is_never_band_4() {
    over_band(4, colour_is_never);
}

#[test]
fn sim_backtrace_is_kept_or_one_band_4() {
    over_band(4, backtrace_is_kept_or_one);
}

#[test]
fn sim_incremental_is_kept_band_5() {
    over_band(5, incremental_is_kept);
}

#[test]
fn sim_incremental_defaults_to_zero_band_5() {
    over_band(5, incremental_defaults_to_zero);
}

#[test]
fn sim_colour_is_never_band_5() {
    over_band(5, colour_is_never);
}

#[test]
fn sim_backtrace_is_kept_or_one_band_5() {
    over_band(5, backtrace_is_kept_or_one);
}

#[test]
fn sim_incremental_is_kept_band_6() {
    over_band(6, incremental_is_kept);
}

#[test]
fn sim_incremental_defaults_to_zero_band_6() {
    over_band(6, incremental_defaults_to_zero);
}

#[test]
fn sim_colour_is_never_band_6() {
    over_band(6, colour_is_never);
}

#[test]
fn sim_backtrace_is_kept_or_one_band_6() {
    over_band(6, backtrace_is_kept_or_one);
}

#[test]
fn sim_incremental_is_kept_band_7() {
    over_band(7, incremental_is_kept);
}

#[test]
fn sim_incremental_defaults_to_zero_band_7() {
    over_band(7, incremental_defaults_to_zero);
}

#[test]
fn sim_colour_is_never_band_7() {
    over_band(7, colour_is_never);
}

#[test]
fn sim_backtrace_is_kept_or_one_band_7() {
    over_band(7, backtrace_is_kept_or_one);
}

#[test]
fn sim_incremental_is_kept_band_8() {
    over_band(8, incremental_is_kept);
}

#[test]
fn sim_incremental_defaults_to_zero_band_8() {
    over_band(8, incremental_defaults_to_zero);
}

#[test]
fn sim_colour_is_never_band_8() {
    over_band(8, colour_is_never);
}

#[test]
fn sim_backtrace_is_kept_or_one_band_8() {
    over_band(8, backtrace_is_kept_or_one);
}

#[test]
fn sim_incremental_is_kept_band_9() {
    over_band(9, incremental_is_kept);
}

#[test]
fn sim_incremental_defaults_to_zero_band_9() {
    over_band(9, incremental_defaults_to_zero);
}

#[test]
fn sim_colour_is_never_band_9() {
    over_band(9, colour_is_never);
}

#[test]
fn sim_backtrace_is_kept_or_one_band_9() {
    over_band(9, backtrace_is_kept_or_one);
}

#[test]
fn sim_incremental_is_kept_band_10() {
    over_band(10, incremental_is_kept);
}

#[test]
fn sim_incremental_defaults_to_zero_band_10() {
    over_band(10, incremental_defaults_to_zero);
}

#[test]
fn sim_colour_is_never_band_10() {
    over_band(10, colour_is_never);
}

#[test]
fn sim_backtrace_is_kept_or_one_band_10() {
    over_band(10, backtrace_is_kept_or_one);
}

#[test]
fn sim_incremental_is_kept_band_11() {
    over_band(11, incremental_is_kept);
}

#[test]
fn sim_incremental_defaults_to_zero_band_11() {
    over_band(11, incremental_defaults_to_zero);
}

#[test]
fn sim_colour_is_never_band_11() {
    over_band(11, colour_is_never);
}

#[test]
fn sim_backtrace_is_kept_or_one_band_11() {
    over_band(11, backtrace_is_kept_or_one);
}

#[test]
fn sim_incremental_is_kept_band_12() {
    over_band(12, incremental_is_kept);
}

#[test]
fn sim_incremental_defaults_to_zero_band_12() {
    over_band(12, incremental_defaults_to_zero);
}

#[test]
fn sim_colour_is_never_band_12() {
    over_band(12, colour_is_never);
}

#[test]
fn sim_backtrace_is_kept_or_one_band_12() {
    over_band(12, backtrace_is_kept_or_one);
}

#[test]
fn sim_incremental_is_kept_band_13() {
    over_band(13, incremental_is_kept);
}

#[test]
fn sim_incremental_defaults_to_zero_band_13() {
    over_band(13, incremental_defaults_to_zero);
}

#[test]
fn sim_colour_is_never_band_13() {
    over_band(13, colour_is_never);
}

#[test]
fn sim_backtrace_is_kept_or_one_band_13() {
    over_band(13, backtrace_is_kept_or_one);
}

#[test]
fn sim_incremental_is_kept_band_14() {
    over_band(14, incremental_is_kept);
}

#[test]
fn sim_incremental_defaults_to_zero_band_14() {
    over_band(14, incremental_defaults_to_zero);
}

#[test]
fn sim_colour_is_never_band_14() {
    over_band(14, colour_is_never);
}

#[test]
fn sim_backtrace_is_kept_or_one_band_14() {
    over_band(14, backtrace_is_kept_or_one);
}

#[test]
fn sim_incremental_is_kept_band_15() {
    over_band(15, incremental_is_kept);
}

#[test]
fn sim_incremental_defaults_to_zero_band_15() {
    over_band(15, incremental_defaults_to_zero);
}

#[test]
fn sim_colour_is_never_band_15() {
    over_band(15, colour_is_never);
}

#[test]
fn sim_backtrace_is_kept_or_one_band_15() {
    over_band(15, backtrace_is_kept_or_one);
}

#[test]
fn sim_incremental_is_kept_band_16() {
    over_band(16, incremental_is_kept);
}

#[test]
fn sim_incremental_defaults_to_zero_band_16() {
    over_band(16, incremental_defaults_to_zero);
}

#[test]
fn sim_colour_is_never_band_16() {
    over_band(16, colour_is_never);
}

#[test]
fn sim_backtrace_is_kept_or_one_band_16() {
    over_band(16, backtrace_is_kept_or_one);
}

#[test]
fn sim_incremental_is_kept_band_17() {
    over_band(17, incremental_is_kept);
}

#[test]
fn sim_incremental_defaults_to_zero_band_17() {
    over_band(17, incremental_defaults_to_zero);
}

#[test]
fn sim_colour_is_never_band_17() {
    over_band(17, colour_is_never);
}

#[test]
fn sim_backtrace_is_kept_or_one_band_17() {
    over_band(17, backtrace_is_kept_or_one);
}

#[test]
fn sim_incremental_is_kept_band_18() {
    over_band(18, incremental_is_kept);
}

#[test]
fn sim_incremental_defaults_to_zero_band_18() {
    over_band(18, incremental_defaults_to_zero);
}

#[test]
fn sim_colour_is_never_band_18() {
    over_band(18, colour_is_never);
}

#[test]
fn sim_backtrace_is_kept_or_one_band_18() {
    over_band(18, backtrace_is_kept_or_one);
}
