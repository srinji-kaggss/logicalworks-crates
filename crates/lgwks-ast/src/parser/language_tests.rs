//! Extension and shebang resolution and registered grammars, under test.

use super::*;

#[test]
fn path_extension_selects_a_language() {
    assert_eq!(Language::of_path("a/b.rs"), Some(Language::Rust));
    assert_eq!(Language::of_path("a/B.RS"), Some(Language::Rust));
    assert_eq!(Language::of_path("noextension"), None);
    assert_eq!(Language::of_path("a/b.cobol"), None);
}

#[test]
fn every_declared_extension_resolves_to_its_own_language() {
    for &language in Language::ALL {
        for extension in language.extensions() {
            assert_eq!(
                Language::of_path(&format!("dir/file.{extension}")),
                Some(language),
                "{}: .{extension} must resolve to itself",
                language.name()
            );
        }
    }
}

#[test]
fn no_two_enabled_languages_claim_one_extension() {
    let mut claimed: Vec<(&str, &str)> = Vec::new();
    for &language in Language::ALL {
        // `&extension` binds the `&str` itself rather than the `&&str` a
        // bare binding would take, so the comparison below is `&str` to
        // `&str` and no pattern sits between the reference and the value.
        for &extension in language.extensions() {
            let previous = claimed
                .iter()
                .find(|entry| entry.0 == extension)
                .map(|entry| entry.1);
            assert_eq!(
                previous,
                None,
                ".{extension} is claimed by {} and by an earlier language",
                language.name()
            );
            claimed.push((extension, language.name()));
        }
    }
}

/// Assert a shebang resolves to Rust.
///
/// `Rust` is in the default grammar set, so it exists in every feature
/// configuration the grammar matrix builds. Naming `Language::Python` here
/// compiled only where `lang-python` was on, and the `lang-rust`-alone
/// build failed on it — a test that states the property under test, rather
/// than which grammars happen to be present.
fn assert_shebang_is_rust(line: &str) {
    assert_eq!(
        Language::of_shebang(line),
        Some(Language::Rust),
        "{line:?} names Rust"
    );
}

#[test]
fn a_shebang_names_the_language_of_an_extensionless_file() {
    // The case the crates.io description promises: a script with no
    // extension to read.
    assert_shebang_is_rust("#!/usr/bin/env rust\n");
    // A version-pinned interpreter names the same language.
    assert_shebang_is_rust("#!/usr/bin/rust\n");
    // `rustc` is a Rust *tool*, not the interpreter the table claims,
    // and the trailing `c` is not a version suffix. Resolving it would
    // attach a grammar to a program that compiles it rather than runs it.
    assert_eq!(Language::of_shebang("#!/usr/bin/env rustc\n"), None);
    // `node` is a runtime name, not a language name this table claims.
    // Guessing here would attach a grammar to a name the caller never gave.
    assert_eq!(Language::of_shebang("#!/usr/bin/env node\n"), None);
    // An interpreter with no compiled grammar is unknown, not a guess.
    assert_eq!(Language::of_shebang("#!/bin/sh\n"), None);
    // No shebang at all.
    assert_eq!(Language::of_shebang("fn main() {}\n"), None);
    // A `#!` that is not at the start is a comment, not a shebang.
    assert_eq!(Language::of_shebang(" #!/usr/bin/env rust\n"), None);
}

#[test]
fn an_interpreter_flag_is_not_part_of_the_language_name() {
    // `-u`, `-Es` and friends are options to the interpreter, not part of
    // its name. `#!/usr/bin/python3 -u` runs python.
    // `Rust` is in the default grammar set, so this holds in every
    // feature configuration the grammar matrix builds. Naming `Python`
    // here would compile only with `lang-python` enabled.
    let rust = Some(Language::Rust);
    assert_eq!(Language::of_shebang("#!/usr/bin/rust -u\n"), rust);
    // `env` takes the interpreter as its argument, and may itself take a
    // `-S` style option before it, spelled either way in the wild, plus a
    // flag of its own. All three shapes must peel to the same interpreter.
    for line in [
        "#!/usr/bin/env -S rust -u\n",
        "#!/usr/bin/env -Srust\n",
        "#!/usr/bin/env -i rust\n",
    ] {
        assert_eq!(
            Language::of_shebang(line),
            Some(Language::Rust),
            "{line:?} must resolve to Rust"
        );
    }
}

#[test]
fn a_shebang_is_bounded_before_it_is_read() {
    // A binary whose first line is a megabyte of non-newline bytes must be
    // refused in constant time, not scanned.
    let long = format!("#!{}\n", "/usr/bin/".repeat(MAX_SHEBANG_BYTES));
    assert!(long.len() > MAX_SHEBANG_BYTES);
    assert_eq!(Language::of_shebang(&long), None);
}

#[test]
fn an_empty_shebang_is_not_a_language() {
    assert_eq!(Language::of_shebang("#!\n"), None);
    assert_eq!(Language::of_shebang("#!"), None);
    assert_eq!(Language::of_shebang("#! \n"), None);
}

#[test]
fn a_shebang_and_a_path_resolve_to_the_same_language() {
    // One table, not two that can disagree: `deploy` and `deploy.rs` must
    // not resolve to different languages. Rust is default-on, so this holds
    // in every feature set the grammar matrix builds; naming `.py` made the
    // `lang-rust`-alone build fail to compile.
    let by_path = detect("deploy.rs");
    let by_shebang = Language::of_shebang("#!/usr/bin/env rust\n");
    assert_eq!(by_path, by_shebang);
    assert!(by_shebang.is_some(), "rust is default-on and must resolve");
}

#[test]
fn an_unknown_interpreter_names_no_language() {
    assert_eq!(
        Language::of_shebang("#!/usr/bin/env not-a-real-interpreter\n"),
        None
    );
}

#[test]
fn name_and_grammar_agree_for_every_enabled_language() {
    for &language in Language::ALL {
        assert!(!language.name().is_empty());
        assert!(!language.extensions().is_empty());
        // Constructing the grammar must not panic for any enabled row.
        let _ = language.support_lang();
    }
}

#[test]
fn a_registered_grammar_parses_through_the_bounded_api() -> Result<(), ParseError> {
    let custom = CustomLang::new("rust-as-custom", SupportLang::Rust.get_ts_language());
    let parsed = try_parse_with("fn f() {}", &custom, custom.name())?;
    assert!(max_depth(&parsed.root()) > 1);
    assert!(!has_syntax_issues(&parsed.root()));
    Ok(())
}

#[test]
fn a_registered_grammar_keeps_the_recovery_refusal() {
    let custom = CustomLang::new("rust-as-custom", SupportLang::Rust.get_ts_language());
    assert!(matches!(
        try_parse_with("fn f( {", &custom, custom.name()),
        Err(ParseError::InvalidSyntax { .. })
    ));
}

#[test]
fn a_registered_grammar_refuses_oversized_source() {
    let custom = CustomLang::new("rust-as-custom", SupportLang::Rust.get_ts_language());
    let big = "x".repeat(MAX_SOURCE_BYTES + 1);
    assert!(matches!(
        try_parse_with(&big, &custom, custom.name()),
        Err(ParseError::SourceTooLarge { .. })
    ));
}

#[test]
fn registered_grammars_resolve_paths_by_extensions() {
    let custom = CustomLang::new("rust-as-custom", SupportLang::Rust.get_ts_language())
        .with_extensions(&["custom", "CU"]);
    assert_eq!(
        CustomLang::of_path("a/file.custom", std::slice::from_ref(&custom))
            .map(|language| language.name()),
        Some("rust-as-custom")
    );
    assert_eq!(
        CustomLang::of_path("a/file.cu", std::slice::from_ref(&custom))
            .map(|language| language.name()),
        Some("rust-as-custom")
    );
    assert!(CustomLang::of_path("a/file.rs", &[custom]).is_none());
}
