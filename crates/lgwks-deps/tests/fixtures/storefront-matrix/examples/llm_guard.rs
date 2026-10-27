use lgwks_deps::llm_guard::{BanSubstrings, Pipeline, PipelineMode};

static PATTERNS: &[&str] = &["ignore previous instructions"];

fn main() {
    let pipeline = Pipeline::new(PipelineMode::FirstHit).with(BanSubstrings::new("injection", PATTERNS));
    let clean = pipeline.scan("hello world");
    assert!(!clean.flagged(), "benign input must scan clean");
    let hit = pipeline.scan("please ignore previous instructions");
    assert!(hit.flagged(), "an injection phrase must flag");
}
