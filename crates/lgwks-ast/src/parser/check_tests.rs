//! The checked parse, content detection and the bounds, under test (INV-AST-1, INV-AST-2, INV-AST-4).

#[cfg(feature = "lang-python")]
use super::check::detect_candidates;
use super::check::{Bounds, parse_bounded};
use super::walk::inspect_ast_with_pending;
use super::*;

#[test]
fn checked_parse_accepts_valid_rust() -> Result<(), ParseError> {
    // A test that returns `Result` reports the refusal's own `Debug` on
    // failure, which is the same report `.expect` would have produced,
    // without an `expect` in the tree.
    let parsed = try_parse("fn f() {}", Language::Rust)?;
    assert!(max_depth(&parsed.root()) > 1);
    assert!(!has_syntax_issues(&parsed.root()));
    Ok(())
}

#[test]
fn recovery_nodes_refuse_before_consumers() {
    assert!(matches!(
        try_parse("fn f( {", Language::Rust),
        Err(ParseError::InvalidSyntax { .. })
    ));
}

#[test]
fn oversized_source_refuses_before_parsing() {
    let big = "x".repeat(MAX_SOURCE_BYTES + 1);
    assert!(matches!(
        try_parse(&big, Language::Rust),
        Err(ParseError::SourceTooLarge { .. })
    ));
}

#[test]
fn detect_is_extension_only_and_never_trial_parses() {
    assert_eq!(detect("probe.rs"), Some(Language::Rust));
    assert_eq!(detect("noextension"), None);
}

#[test]
fn content_detection_is_opt_in_and_capped_below_the_parse_bound() {
    assert_eq!(
        try_detect_content("fn f() {}", &[Language::Rust]),
        Ok(ContentDetection::Unique(Language::Rust))
    );
    // A probe past MAX_DETECT_BYTES is refused before any grammar runs, so
    // content detection costs at most candidates x MAX_DETECT_BYTES even
    // though the checked parse bound is MAX_SOURCE_BYTES.
    let oversized_probe = "x".repeat(MAX_DETECT_BYTES + 1);
    assert!(matches!(
        try_detect_content(&oversized_probe, &[Language::Rust]),
        Err(ParseError::SourceTooLarge { .. })
    ));
}

#[test]
fn content_detection_needs_exactly_one_clean_reading() {
    assert_eq!(
        try_detect_content("fn f() {}", &[]),
        Ok(ContentDetection::NoMatch)
    );
    assert_eq!(
        try_detect_content("fn f() {}", &[Language::Rust, Language::Rust]),
        Ok(ContentDetection::Unique(Language::Rust))
    );
}

#[test]
#[cfg(feature = "lang-python")]
fn duplicate_and_permuted_candidates_parse_once_and_preserve_ambiguity() {
    let candidate_orders = [
        [Language::Rust, Language::Rust, Language::Python],
        [Language::Python, Language::Rust, Language::Rust],
        [Language::Rust, Language::Python, Language::Rust],
    ];
    for candidates in candidate_orders {
        let mut attempts = Vec::new();
        let result = detect_candidates(&candidates, |language| {
            attempts.push(language);
            if language == Language::Rust {
                Ok(())
            } else {
                Err(ParseError::InvalidSyntax {
                    language: language.name(),
                    diagnostics: Vec::new(),
                    diagnostics_truncated: false,
                })
            }
        });
        assert_eq!(
            result,
            Ok(ContentDetection::Unique(Language::Rust)),
            "order preserves the unique result"
        );
        assert_eq!(attempts.len(), 2, "each distinct candidate is parsed once");
        assert_eq!(
            attempts
                .iter()
                .filter(|&&item| item == Language::Rust)
                .count(),
            1,
            "repeated Rust candidates do not repeat parsing"
        );
    }

    let ambiguous_candidates = [Language::Rust, Language::Python, Language::Rust];
    let mut attempts = Vec::new();
    let ambiguous = detect_candidates(&ambiguous_candidates, |language| {
        attempts.push(language);
        Ok(())
    });
    assert_eq!(
        ambiguous,
        Ok(ContentDetection::Ambiguous),
        "two distinct clean readings remain ambiguous"
    );
    assert_eq!(
        attempts.len(),
        2,
        "duplicate candidates are not parsed twice"
    );
}

#[test]
#[cfg(feature = "lang-python")]
fn incomplete_candidate_inspection_is_not_reported_as_unique() {
    let candidates = [Language::Rust, Language::Python];
    let unavailable = ParseError::ParserUnavailable {
        language: "python",
        detail: "injected parser failure".to_owned(),
    };
    let result = detect_candidates(&candidates, |language| {
        if language == Language::Rust {
            Ok(())
        } else {
            Err(unavailable.clone())
        }
    });
    assert_eq!(
        result,
        Err(unavailable),
        "an unavailable candidate blocks uniqueness"
    );

    let over_budget = ParseError::AstTooLarge {
        language: "python",
        observed: 3,
        limit: 2,
    };
    let result = detect_candidates(&candidates, |language| {
        if language == Language::Rust {
            Ok(())
        } else {
            Err(over_budget.clone())
        }
    });
    assert_eq!(
        result,
        Err(over_budget),
        "a budget refusal blocks uniqueness"
    );

    let reversed = [Language::Python, Language::Rust];
    let first_order_error = detect_candidates(&candidates, |language| {
        Err(ParseError::ParserUnavailable {
            language: language.name(),
            detail: "injected failure".to_owned(),
        })
    });
    let reversed_order_error = detect_candidates(&reversed, |language| {
        Err(ParseError::ParserUnavailable {
            language: language.name(),
            detail: "injected failure".to_owned(),
        })
    });
    assert_eq!(
        first_order_error, reversed_order_error,
        "candidate permutation preserves the deterministic incomplete result"
    );
    assert!(
        matches!(
            first_order_error,
            Err(ParseError::ParserUnavailable {
                language: "python",
                ..
            })
        ),
        "the lexically first failed candidate owns the stable refusal"
    );
}

#[test]
fn over_budget_tree_refuses_as_ast_too_large() {
    // parse_bounded is the one place the node bound is enforced, so drive it
    // directly with a bound small enough to exceed without a megabyte of
    // source. The walk stops at limit + 1, which is what proves refusal.
    let refusal = parse_bounded(
        "fn f() { let x = 1; }",
        &SupportLang::Rust,
        "rust",
        Bounds {
            source_bytes: MAX_SOURCE_BYTES,
            ast_nodes: 2,
            ast_depth: MAX_AST_DEPTH,
            deadline: DEFAULT_PARSE_DEADLINE,
        },
    );
    // The gate is this assertion, which reports whatever came back
    // instead. The `if let` below only reads the numbers, and the pattern
    // is the same one asserted here, so it always matches when the test
    // reaches it.
    assert!(
        matches!(refusal, Err(ParseError::AstTooLarge { .. })),
        "a tree past the node bound must refuse as AstTooLarge, got {:?}",
        // `as_ref().err()` rather than the whole `Result`: a successful
        // parse holds a `Parsed`, which is not `Debug`.
        refusal.as_ref().err()
    );
    if let Err(ParseError::AstTooLarge {
        observed, limit, ..
    }) = refusal
    {
        assert_eq!(limit, 2);
        assert!(observed > limit, "observed {observed} must exceed {limit}");
    }
}

#[test]
fn a_tree_past_the_depth_bound_refuses_as_ast_too_deep() {
    // The other half of the checked parse's two tree bounds, and the half
    // `over_budget_tree_refuses_as_ast_too_large` cannot reach: that fixture
    // is wide and shallow, so it trips the node ceiling and says nothing
    // about depth. This one is narrow and deep, which is the adversarial
    // shape — a few bytes per nesting level, so the byte ceiling admits the
    // whole source and the node ceiling never charges it.
    let refusal = parse_bounded(
        &format!("{}fn f() {{}}{}", "fn f() {".repeat(64), "}".repeat(64)),
        &SupportLang::Rust,
        "rust",
        Bounds {
            source_bytes: MAX_SOURCE_BYTES,
            ast_nodes: MAX_AST_NODES,
            ast_depth: 8,
            deadline: DEFAULT_PARSE_DEADLINE,
        },
    );
    assert!(
        matches!(refusal, Err(ParseError::AstTooDeep { .. })),
        "a tree past the depth bound must refuse as AstTooDeep, got {:?}",
        refusal.as_ref().err()
    );
    if let Err(ParseError::AstTooDeep {
        observed, limit, ..
    }) = refusal
    {
        assert_eq!(limit, 8);
        assert_eq!(
            observed, 9,
            "the depth witness is one level past the ceiling, like the node one"
        );
    }
}

#[test]
fn the_depth_witness_costs_one_level_more_than_the_ceiling_and_no_more() {
    // The resource half of `MAX_AST_DEPTH`: the retained cursor stack is the
    // memory the ceiling exists to bound, so the deepest the walk ever holds
    // is the ceiling plus the one witness it has to visit to be able to say
    // the tree is too deep. Before #277 the frame walk stopped *before*
    // pushing the witness's frame, so it held exactly the ceiling; the
    // cursor has already descended to reach the witness, so it holds the
    // witness too. Both are one frame, and the bound is on the order rather
    // than the identity: what must not happen is a stack that grows with
    // the tree.
    let parsed = parse(
        &format!("{}fn f() {{}}{}", "fn f() {".repeat(64), "}".repeat(64)),
        Language::Rust,
    );
    let root = parsed.root();
    let (metrics, peak_depth, _, _) = inspect_ast_with_pending(&root, Some(MAX_AST_NODES), Some(8));
    assert_eq!(
        metrics.stop_reason,
        Some(InspectionStopReason::DepthLimitExceeded),
        "the metrics name the depth ceiling as the reason"
    );
    assert_eq!(
        peak_depth, 9,
        "the witness is visited, so the stack is the ceiling plus it"
    );
    assert_eq!(
        metrics.max_depth, 9,
        "and the reported depth is the same witness, not a constant"
    );
    assert!(
        !metrics.complete,
        "a walk stopped at the depth ceiling did not finish"
    );

    // The same tree, walked to completion, is where the bound would show if
    // it were really the tree's depth: this is the control that reads a
    // ceiling as a ceiling rather than as the whole tree.
    let (unbounded, unbounded_peak, _, _) =
        inspect_ast_with_pending(&root, Some(MAX_AST_NODES), None);
    assert!(
        unbounded.complete,
        "the uncapped walk descends the whole tree"
    );
    assert!(
        unbounded_peak > 64,
        "the fixture is {} deep, so a bounded walk holding only 9 levels is \
         the ceiling and not the tree",
        unbounded_peak
    );
}

#[test]
fn the_unbounded_inspection_measures_the_real_depth_of_a_deep_tree() {
    // Why the depth ceiling is the checked parse's and not `inspect_ast`'s:
    // a caller inspecting a malformed tree on purpose still learns how deep
    // it is, rather than reading back the ceiling. 512 nestings of a Rust
    // block is about 1 030 levels of node depth, measured rather than
    // assumed — which is also why the ceiling is 512 and not 64.
    let nestings = 512;
    let parsed = parse(
        &format!(
            "{}fn f() {{}}{}",
            "fn f() {".repeat(nestings),
            "}".repeat(nestings)
        ),
        Language::Rust,
    );
    let metrics = inspect_ast(&parsed.root(), None);
    assert!(metrics.complete, "an uncapped walk descends the whole tree");
    assert_eq!(metrics.stop_reason, None, "nothing stopped it");
    assert!(
        metrics.max_depth > MAX_AST_DEPTH,
        "the fixture is {} deep, which is not past the checked parse's ceiling of {MAX_AST_DEPTH}",
        metrics.max_depth
    );
    assert!(
        try_parse(
            &format!(
                "{}fn f() {{}}{}",
                "fn f() {".repeat(nestings),
                "}".repeat(nestings)
            ),
            Language::Rust
        )
        .is_err(),
        "and the same source is refused by the checked parse"
    );
}

#[test]
fn the_container_count_names_the_shapes_the_scanner_overflows_on()
-> Result<(), Box<dyn std::error::Error>> {
    // The counting rules against the sources the scanner was measured to
    // abort on: at that depth the guard must refuse, and at a depth inside
    // the bound it must not. The count is the number of containers the
    // source opens, which is why the shapes nesting two or three per
    // repetition cross the bound in a quarter or a third of the depth.
    //
    // (fragment, containers per repetition, the repetition that aborts)
    let shapes: [(&str, usize, usize); 8] = [
        ("- ", 1, 255),
        ("> ", 1, 255),
        (">", 1, 255),
        ("1. ", 1, 255),
        ("1) ", 1, 255),
        ("> - ", 2, 128),
        (">>> ", 3, 85),
        ("- > - ", 3, 85),
    ];
    let limit = MAX_MARKDOWN_CONTAINERS_PER_LINE;
    for (fragment, per_repetition, aborts_at) in shapes {
        let inside = limit.saturating_div(per_repetition);
        assert!(
            markdown_containers(&format!("{}x\n", fragment.repeat(inside)), limit).is_none(),
            "{fragment:?} at {inside} repetitions opens {} containers, inside the bound of \
             {limit}, and must pass",
            per_repetition.saturating_mul(inside)
        );
        let Some(counted) =
            markdown_containers(&format!("{}x\n", fragment.repeat(aborts_at)), limit)
        else {
            return Err(format!(
                "{fragment:?} at {aborts_at} repetitions sits inside the bound of {limit}, so it \
                 cannot exercise the margin the guard keeps"
            )
            .into());
        };
        assert!(
            counted >= per_repetition.saturating_mul(aborts_at),
            "{fragment:?} at {aborts_at} repetitions opens {} containers, which the \
             scanner cannot serialize, but the count said {counted}",
            per_repetition.saturating_mul(aborts_at)
        );
    }
    Ok(())
}

#[test]
fn the_container_count_does_not_charge_prose_or_a_thematic_break() {
    // The over-count is deliberate and bounded, but the *under*-count would
    // be a crash, so the things that are not containers must not be read as
    // containers and the things that are must not be missed.
    let limit = MAX_MARKDOWN_CONTAINERS_PER_LINE;
    for source in [
        "# Title\n\nSome prose with a -- dash and a 1.5 number.\n",
        "***\n\n___\n\n- - -\n",
        "| a | b |\n|---|---|\n| 1 | 2 |\n",
        "*emphasis* and _underscore_ and `code`\n",
        "```rust\nfn main() {}\n```\n",
        "See https://example.com/a?q=1> for the details.\n",
    ] {
        assert!(
            markdown_containers(source, limit).is_none(),
            "{source:?} was read as deeply nested containers"
        );
    }
    // And the shapes that are containers, read as containers.
    assert_eq!(markdown_containers("> ", 0), Some(1), "a blockquote is one");
    assert_eq!(markdown_containers("- ", 0), Some(1), "a list item is one");
    assert_eq!(
        markdown_containers("1) ", 0),
        Some(1),
        "an ordered item is one"
    );
    assert_eq!(
        markdown_containers("```\n", 0),
        Some(1),
        "a fence is one container"
    );
    assert_eq!(
        markdown_containers(&" ".repeat(8), 0),
        Some(4),
        "eight columns of indent is four levels of container"
    );
    assert_eq!(
        markdown_containers("\t\t", 0),
        Some(4),
        "two tabs are eight columns, which is also four levels"
    );
}

#[test]
fn the_container_count_is_linear_in_the_bytes_and_holds_no_line() {
    // The bound is a process-safety bound on untrusted input, so its own cost
    // has to be bounded too: one pass, and no allocation proportional to the
    // source. 4 MiB is past the crate's byte ceiling, which is the point --
    // the counter is what a caller runs *instead of* the parser when the
    // source is too large for it.
    let wide = "> ".repeat(2 * 1024 * 1024);
    let started = std::time::Instant::now();
    let counted = markdown_containers(&wide, MAX_MARKDOWN_CONTAINERS_PER_LINE);
    let elapsed = started.elapsed();
    assert!(
        counted.is_some(),
        "a 4 MiB run of blockquote markers is refused"
    );
    assert!(
        elapsed.as_secs() < 30,
        "counting 4 MiB took {elapsed:?}; the guard must be cheaper than the parse it \
         replaces"
    );
}

#[test]
fn a_refusal_names_the_container_bound_and_where_it_was_applied() {
    // The refusal is a pre-parse answer, so it must name the bound and the
    // count and render as a report a caller can show.
    let refusal = ParseError::ContainerNestingTooDeep {
        language: "markdown",
        observed: 255,
        limit: MAX_MARKDOWN_CONTAINERS_PER_LINE,
    };
    assert_eq!(
        refusal.to_string(),
        "markdown declares at least 255 block containers on one line; the limit is 64",
        "the refusal names the grammar, the count and the bound in one line"
    );
    let rendered = refusal.to_diagnostic("README.md", "- x\n");
    assert!(
        rendered.render().starts_with("README.md:"),
        "the rendered refusal is located: {}",
        rendered.render()
    );
    assert!(
        rendered.message().contains("255") && rendered.message().contains("64"),
        "the refusal names the observed count and the bound: {}",
        rendered.message()
    );
}

#[test]
fn a_zero_deadline_stops_a_nested_parse_and_frees_the_thread() {
    let nested = "fn f() {".repeat(4_000);
    let refusal = try_parse_within(&nested, Language::Rust, Duration::ZERO);
    assert!(
        matches!(refusal, Err(ParseError::TimedOut { .. })),
        "a parse past its deadline answers TimedOut"
    );
    assert!(
        try_parse_within("fn f() {}", Language::Rust, Duration::from_secs(1)).is_ok(),
        "the thread is free, and its parser starts clean"
    );
}
