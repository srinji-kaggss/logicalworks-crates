//! The `drishti` command, driven as a user and an agent drive it: the built
//! binary, its JSON on stdout, its refusals on stderr and its exit status.
//!
//! Sources: the replay requirement of `docs/ink-engine.md` (one seed, one
//! geometry), the device-epoch contract of `docs/gpu-runtime.md` (no receipt
//! from an old epoch after recovery), WCAG 2.2 §1.4.3 for the contrast table,
//! and the estate rule that every default serves an agent (`--json`) with no
//! mode switch.

use std::process::{Command, Output};

use lgwks_std::json::{self, Value};

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// Run the shipped binary with `args`.
fn drishti(args: &[&str]) -> Result<Output, std::io::Error> {
    Command::new(env!("CARGO_BIN_EXE_drishti"))
        .args(args)
        .output()
}

/// Run it and parse a successful document.
fn document(args: &[&str]) -> Result<Value, Box<dyn std::error::Error>> {
    let output = drishti(args)?;
    assert!(output.status.success(), "{args:?} failed: {output:?}");
    Ok(json::from_slice(&output.stdout)?)
}

/// Every effect's digest, in brand order.
fn digests(document: &Value) -> Vec<String> {
    document["effects"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|effect| effect["digest"].to_string())
        .collect()
}

#[test]
fn ink_replays_by_seed_and_stays_inside_its_bounds() -> TestResult {
    let first = document(&["ink", "--seed", "7", "--frames", "90"])?;
    let again = document(&["ink", "--seed", "7", "--frames", "90"])?;
    let other = document(&["ink", "--seed", "8", "--frames", "90"])?;
    assert_eq!(digests(&first).len(), 11, "every effect ran");
    assert_eq!(digests(&first), digests(&again), "one seed, one geometry");
    assert_ne!(
        digests(&first),
        digests(&other),
        "the seed reaches the geometry"
    );
    for effect in first["effects"].as_array().into_iter().flatten() {
        assert_eq!(effect["within_bound"], true, "{effect}");
        // A finished effect stops demanding frames (reveal settles near frame
        // 62), so every effect rendered at least once and never past the run.
        let rendered = effect["rendered"].as_u64().ok_or("no count")?;
        assert!((1..=90).contains(&rendered), "{effect}");
        assert!(effect.get("refused").is_none(), "{effect}");
    }
    Ok(())
}

#[test]
fn reduced_motion_resolves_the_mark_and_the_count() -> TestResult {
    let resolved = document(&["ink", "--reduce-motion", "--frames", "2"])?;
    let effects = resolved["effects"].as_array().ok_or("no effects")?;
    let named = |name: &str| effects.iter().find(|effect| effect["effect"] == name);
    let mark = named("mark").ok_or("no mark")?;
    assert_eq!(mark["peak_shapes"], 24, "the published twenty-four beams");
    let count = named("count").ok_or("no count")?;
    assert_eq!(count["label"], "$12,480.5", "grouped, prefixed, settled");
    Ok(())
}

#[test]
fn a_session_survives_loss_and_never_presents_a_stale_receipt() -> TestResult {
    let session = document(&["present", "--seed", "3", "--frames", "120"])?;
    assert_eq!(session["hosts"], 11);
    assert_eq!(session["refused_setup"].as_array().map(Vec::len), Some(0));
    assert_eq!(
        session["epochs"],
        json::json!([1, 2]),
        "one recovery, one bump"
    );
    assert_eq!(
        session["refused_unavailable"], 2,
        "draining and lost refuse"
    );
    assert_eq!(session["stale_receipts"], 0);
    assert_eq!(session["state"], "ready");
    assert_eq!(session["totals"]["frames_presented"], 120);
    assert_eq!(session["present_ms"]["samples"], 122);
    let again = document(&["present", "--seed", "3", "--frames", "120"])?;
    assert_eq!(
        session["digest"], again["digest"],
        "receipts replay by seed"
    );
    Ok(())
}

#[test]
fn tokens_publish_every_role_in_both_themes_with_grades() -> TestResult {
    let tokens = document(&["tokens"])?;
    assert_eq!(tokens["primitives"].as_array().map(Vec::len), Some(18));
    for primitive in tokens["primitives"].as_array().into_iter().flatten() {
        assert_eq!(primitive["round_trips"], true, "{primitive}");
    }
    assert_eq!(tokens["colourways"].as_array().map(Vec::len), Some(9));
    for theme in tokens["themes"].as_array().into_iter().flatten() {
        assert_eq!(theme["roles"].as_array().map(Vec::len), Some(18));
        // Body text on the canvas meets AAA (7:1) in both themes.
        assert_eq!(
            theme["contrast"]["text-on-canvas"]["text"], "AAA",
            "{theme}"
        );
    }
    let css = tokens["css"].as_str().ok_or("no css")?;
    assert!(css.contains("--lw-p-clay-500: #c84025;"), "{css}");
    assert!(css.contains("[data-theme=\"dark\"]"), "{css}");
    Ok(())
}

#[test]
fn every_page_of_the_publication_is_readable() -> TestResult {
    let index = document(&["docs"])?;
    let pages = index["pages"].as_array().ok_or("no index")?;
    assert_eq!(pages.len(), 10);
    for page in pages {
        let name = page["page"].as_str().ok_or("unnamed page")?;
        let read = document(&["docs", name])?;
        let markdown = read["markdown"].as_str().ok_or("no markdown")?;
        assert!(markdown.len() > 500, "{name} is a placeholder");
    }
    Ok(())
}

#[test]
fn limits_report_the_declared_bounds() -> TestResult {
    let limits = document(&["limits"])?;
    assert_eq!(limits["ink"]["max_shapes"], 512);
    assert_eq!(limits["ink"]["max_beads"], 64);
    assert_eq!(limits["runtime"]["conservative"]["max_hosts"], 64);
    Ok(())
}

#[test]
fn malformed_input_is_refused_as_json_with_a_failing_status() -> TestResult {
    for args in [
        &[][..],
        &["bogus"][..],
        &["ink", "--frames", "x"][..],
        &["ink", "--frames", "36001"][..],
        &["ink", "--seed"][..],
        &["present", "--frames", "-1"][..],
        &["docs", "nope"][..],
    ] {
        let output = drishti(args)?;
        assert!(!output.status.success(), "{args:?} was accepted");
        assert!(output.stdout.is_empty(), "{args:?} wrote a document");
        let refusal: Value = json::from_slice(&output.stderr)?;
        assert!(refusal["refused"].is_string(), "{args:?}: {refusal}");
    }
    Ok(())
}
