//! `lgwks_ast` owns the estate's one multi-language AST parser.
//!
//! Code tools in the estate — the safety detectors in `keel-core` and the
//! graph extractor in `code-world-model` — need the same three things: decide
//! a source file's language, select that language's tree-sitter grammar, and
//! walk the resulting syntax tree safely. Before this crate each rebuilt its
//! own language enum, extension table, grammar mapping, and parse loop; that
//! is one concept implemented twice, so it lives here instead.
//!
//! Enforced invariant **INV-AST-ONE-PARSER**: a consumer identifies, selects,
//! and parses through this crate and does not depend on `ast-grep` directly.
//!
//! ## Grammar selection
//!
//! One cargo feature per grammar forwards to `ast-grep-language`. The default
//! enables the seven languages the safety detectors parse; every remaining
//! grammar ast-grep-language ships — C#, CSS, Dart, Elixir, Haskell, HCL,
//! HTML, JSON, Lua, Markdown, Nix, PHP, Ruby, Solidity, YAML, and the rest —
//! is its own opt-in `lang-*` feature, and `full` enables all 28. A language
//! whose feature is off is not in [`Language::ALL`] and is never returned by
//! [`Language::of_path`], so no consumer pays to compile a grammar it cannot
//! select.
//!
//! ## Custom languages
//!
//! ast-grep keeps its built-in set small on purpose; its documented extension
//! point for anything else is a caller-registered parser. [`CustomLang`] is
//! that registration: name the language, hand it a `tree-sitter` grammar, and
//! parse it through [`try_parse_with`] under the same bounds and recovery
//! refusal as a built-in. A grammar the estate needs should be contributed to
//! `ast-grep-language` upstream and the local registration deleted once it
//! ships — this crate never forks upstream's language tables.
//!
//! ## Bounded parsing
//!
//! A recoverable tree-sitter tree is not proof of valid syntax: recovery emits
//! `ERROR` and `MISSING` nodes. [`try_parse`] therefore refuses before any
//! detector sees the tree — oversized bytes, a parser that cannot produce a
//! tree, a tree past [`MAX_AST_NODES`], or one carrying recovery nodes. The
//! unchecked [`parse`] exists for diagnostics and tests that inspect
//! malformed trees on purpose.
//!
//! The two bounds are not interchangeable. [`MAX_SOURCE_BYTES`] bounds the
//! bytes handed to the parser and is what keeps parse work linear in input;
//! [`MAX_AST_NODES`] is measured on the tree *after* tree-sitter has built it,
//! so it bounds the validation walk and every downstream walk, not the
//! parser's own allocation. Neither is a hard memory ceiling.
//!
//! Content sniffing is opt-in for the same reason: [`try_detect_content`]
//! trial-parses each candidate grammar in full, so the caller names a small
//! candidate set and the probe source is held to [`MAX_DETECT_BYTES`]. The
//! extension-only [`detect`] never parses.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

use std::fmt;

use ast_grep_core::Language as CoreLanguage;
use ast_grep_core::matcher::{Pattern, PatternBuilder, PatternError};
use ast_grep_core::tree_sitter::LanguageExt;
pub use ast_grep_core::tree_sitter::{StrDoc, TSLanguage};
pub use ast_grep_core::{AstGrep, Node};
pub use ast_grep_language::SupportLang;

/// One parsed source file, owning its tree. Defaults to a built-in [`Language`];
/// a caller-registered grammar parses to `Parsed<CustomLang>`.
pub type Parsed<L = SupportLang> = AstGrep<StrDoc<L>>;

/// One node of a [`Parsed`] tree.
pub type AstNode<'t, L = SupportLang> = Node<'t, StrDoc<L>>;

/// Largest source admitted to the checked parser (2 MiB). This is the input
/// bound that keeps parse work linear in bytes.
pub const MAX_SOURCE_BYTES: usize = 2 * 1024 * 1024;

/// Largest concrete syntax tree admitted; bounds downstream walks, which a
/// byte bound alone does not price. Measured on the tree after tree-sitter has
/// built it, so it does not cap the parser's own allocation.
pub const MAX_AST_NODES: usize = 2_000_000;

/// Largest probe source admitted to [`try_detect_content`] (64 KiB). Content
/// detection costs one full parse per candidate grammar, so it is bounded far
/// below [`MAX_SOURCE_BYTES`]; a language probe only needs enough bytes to
/// show one clean reading.
pub const MAX_DETECT_BYTES: usize = 64 * 1024;

macro_rules! define_languages {
    ($(
        $variant:ident, $doc:literal, $feature:literal, $name:literal,
        [$($ext:literal),+], $support:ident
    );+ $(;)?) => {
        /// Every language this build can select a grammar for.
        ///
        /// A variant exists only when its `lang-*` feature is enabled; the
        /// compiler enforces that a consumer cannot name a language whose
        /// grammar it did not compile.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum Language {
            $(
                #[doc = $doc]
                #[cfg(feature = $feature)]
                $variant,
            )+
        }

        impl Language {
            /// Every language with a compiled grammar, in table order.
            pub const ALL: &'static [Language] = &[
                $(
                    #[cfg(feature = $feature)]
                    Language::$variant,
                )+
            ];

            /// The lowercase, stable name used in findings and diagnostics.
            pub fn name(self) -> &'static str {
                match self {
                    $(
                        #[cfg(feature = $feature)]
                        Language::$variant => $name,
                    )+
                }
            }

            /// Extensions answered for, without the leading dot.
            pub fn extensions(self) -> &'static [&'static str] {
                match self {
                    $(
                        #[cfg(feature = $feature)]
                        Language::$variant => &[$($ext),+],
                    )+
                }
            }

            /// The ast-grep grammar for this language.
            pub fn support_lang(self) -> SupportLang {
                match self {
                    $(
                        #[cfg(feature = $feature)]
                        Language::$variant => SupportLang::$support,
                    )+
                }
            }
        }
    };
}

define_languages! {
    Bash, "Bash / POSIX shell.", "lang-bash", "bash", ["sh", "bash"], Bash;
    C, "C.", "lang-c", "c", ["c", "h"], C;
    Cpp, "C++.", "lang-cpp", "cpp", ["cpp", "hpp", "cc", "cxx", "hh", "hxx"], Cpp;
    CSharp, "C#.", "lang-csharp", "csharp", ["cs"], CSharp;
    Css, "CSS.", "lang-css", "css", ["css"], Css;
    Dart, "Dart.", "lang-dart", "dart", ["dart"], Dart;
    Elixir, "Elixir.", "lang-elixir", "elixir", ["ex", "exs"], Elixir;
    Go, "Go.", "lang-go", "go", ["go"], Go;
    Haskell, "Haskell.", "lang-haskell", "haskell", ["hs"], Haskell;
    Hcl, "HCL / Terraform.", "lang-hcl", "hcl", ["hcl", "tf"], Hcl;
    Html, "HTML.", "lang-html", "html", ["html", "htm"], Html;
    Java, "Java.", "lang-java", "java", ["java"], Java;
    JavaScript, "JavaScript.", "lang-javascript", "javascript", ["js", "jsx", "mjs", "cjs", "vue", "svelte"], JavaScript;
    Json, "JSON.", "lang-json", "json", ["json"], Json;
    Kotlin, "Kotlin.", "lang-kotlin", "kotlin", ["kt", "kts"], Kotlin;
    Lua, "Lua.", "lang-lua", "lua", ["lua"], Lua;
    Markdown, "Markdown.", "lang-md", "markdown", ["md", "markdown"], Markdown;
    Nix, "Nix.", "lang-nix", "nix", ["nix"], Nix;
    Php, "PHP.", "lang-php", "php", ["php"], Php;
    Python, "Python.", "lang-python", "python", ["py", "pyi"], Python;
    Ruby, "Ruby.", "lang-ruby", "ruby", ["rb"], Ruby;
    Rust, "Rust.", "lang-rust", "rust", ["rs"], Rust;
    Scala, "Scala.", "lang-scala", "scala", ["scala", "sc"], Scala;
    Solidity, "Solidity.", "lang-solidity", "solidity", ["sol"], Solidity;
    Swift, "Swift.", "lang-swift", "swift", ["swift"], Swift;
    Tsx, "TSX (TypeScript + JSX).", "lang-tsx", "tsx", ["tsx"], Tsx;
    TypeScript, "TypeScript.", "lang-typescript", "typescript", ["ts", "mts", "cts"], TypeScript;
    Yaml, "YAML.", "lang-yaml", "yaml", ["yaml", "yml"], Yaml;
}

/// The extension of `path` (the substring after its last `.`), or `None` when
/// it has no dot.
fn extension_of(path: &str) -> Option<&str> {
    path.rsplit_once('.').map(|(_, extension)| extension)
}

/// Whether `extension` equals any entry of `known`, ASCII-case-insensitively.
fn any_extension_matches(known: &[&str], extension: &str) -> bool {
    known
        .iter()
        .any(|candidate| candidate.eq_ignore_ascii_case(extension))
}

impl Language {
    /// Language of a path by extension, or `None` without a compiled grammar.
    /// Case-insensitive without allocating a normalized copy.
    pub fn of_path(path: &str) -> Option<Self> {
        let extension = extension_of(path)?;
        Self::ALL
            .iter()
            .copied()
            .find(|language| any_extension_matches(language.extensions(), extension))
    }
}

// ── Custom languages ────────────────────────────────────────────────────────

/// A tree-sitter grammar registered from outside `ast-grep-language`.
///
/// ast-grep keeps its built-in language set small on purpose; for anything it
/// does not ship, upstream's documented extension point is a caller-registered
/// parser. [`CustomLang`] is that registration for the Rust API: name the
/// language, hand it a `tree-sitter` grammar, and it is usable through
/// [`try_parse_with`] and every bounded walker.
///
/// A grammar the estate needs should be contributed upstream and the local
/// registration deleted once `ast-grep-language` ships it; keeping it behind
/// this type makes that a localized change, not a fork of upstream's tables.
#[derive(Clone)]
pub struct CustomLang {
    name: &'static str,
    grammar: TSLanguage,
    expando: char,
    extensions: &'static [&'static str],
}

impl CustomLang {
    /// Register `grammar` under `name`. `$` is assumed valid in the language's
    /// patterns; use [`CustomLang::with_expando`] when it is not.
    pub fn new(name: &'static str, grammar: TSLanguage) -> Self {
        Self {
            name,
            grammar,
            expando: '$',
            extensions: &[],
        }
    }

    /// Replace the character standing in for `$` while parsing patterns, for a
    /// language where `$` is an identifier character.
    pub fn with_expando(mut self, expando: char) -> Self {
        self.expando = expando;
        self
    }

    /// Declare the file extensions this language answers for.
    pub fn with_extensions(mut self, extensions: &'static [&'static str]) -> Self {
        self.extensions = extensions;
        self
    }

    /// The lowercase, stable name used in findings and diagnostics.
    pub fn name(&self) -> &'static str {
        self.name
    }

    /// Extensions answered for, without the leading dot.
    pub fn extensions(&self) -> &'static [&'static str] {
        self.extensions
    }

    /// The first candidate claiming `path`'s extension, case-insensitively.
    /// Registered grammars are selected explicitly; they never join
    /// [`Language::ALL`] or content detection.
    pub fn of_path(path: &str, candidates: &[CustomLang]) -> Option<CustomLang> {
        let extension = extension_of(path)?;
        candidates
            .iter()
            .find(|language| any_extension_matches(language.extensions, extension))
            .cloned()
    }
}

impl CoreLanguage for CustomLang {
    fn expando_char(&self) -> char {
        self.expando
    }

    fn kind_to_id(&self, kind: &str) -> u16 {
        self.grammar.id_for_node_kind(kind, true)
    }

    fn field_to_id(&self, field: &str) -> Option<u16> {
        self.grammar.field_id_for_name(field).map(|id| id.get())
    }

    fn build_pattern(&self, builder: &PatternBuilder) -> Result<Pattern, PatternError> {
        builder.build(|src| StrDoc::try_new(src, self.clone()))
    }
}

impl LanguageExt for CustomLang {
    fn get_ts_language(&self) -> TSLanguage {
        self.grammar.clone()
    }
}

/// A checked-parse refusal. None of these may be reported as clean.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    /// The source exceeds the byte bound.
    SourceTooLarge {
        /// Observed length in bytes.
        actual: usize,
        /// The applied bound.
        limit: usize,
    },
    /// The selected grammar could not produce a syntax tree.
    ParserUnavailable {
        /// The language name.
        language: &'static str,
        /// The parser's own detail string.
        detail: String,
    },
    /// The tree carries an `ERROR` or `MISSING` recovery node.
    InvalidSyntax {
        /// The language name.
        language: &'static str,
    },
    /// The tree exceeds the node bound.
    AstTooLarge {
        /// The language name.
        language: &'static str,
        /// Nodes observed up to the bound.
        observed: usize,
        /// The applied bound.
        limit: usize,
    },
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SourceTooLarge { actual, limit } => {
                write!(f, "source is {actual} bytes; parser limit is {limit} bytes")
            }
            Self::ParserUnavailable { language, detail } => write!(
                f,
                "{language} parser could not produce a syntax tree: {detail}"
            ),
            Self::InvalidSyntax { language } => {
                write!(f, "{language} parser produced an ERROR or MISSING node")
            }
            Self::AstTooLarge {
                language,
                observed,
                limit,
            } => write!(
                f,
                "{language} AST exceeds {limit} nodes (observed at least {observed})"
            ),
        }
    }
}

impl std::error::Error for ParseError {}

/// Node count, deepest depth, and recovery state from one traversal.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AstMetrics {
    /// Nodes visited.
    pub nodes: usize,
    /// Deepest branch depth, root counting as 1.
    pub max_depth: usize,
    /// Whether an `ERROR` or `MISSING` node was seen.
    pub has_syntax_issues: bool,
}

impl AstMetrics {
    fn including<L: LanguageExt>(mut self, node: &AstNode<'_, L>, depth: usize) -> Self {
        self.nodes = self.nodes.saturating_add(1);
        self.max_depth = self.max_depth.max(depth);
        self.has_syntax_issues = self.has_syntax_issues || node.is_error() || node.is_missing();
        self
    }
}

/// Identify a language by file extension. `None` means the name does not claim
/// the file — report unscanned, never clean.
///
/// Content sniffing is deliberately not part of this call: it costs one full
/// parse per grammar. A caller that needs it opts in with
/// [`try_detect_content`] and names the few grammars it expects, rather than
/// trial-parsing every language this build carries.
pub fn detect(path: &str) -> Option<Language> {
    Language::of_path(path)
}

/// Identify `source` by trial-parsing `candidates` — the opt-in content path.
///
/// Cost is `candidates.len()` full parses, so `source` is held to
/// [`MAX_DETECT_BYTES`] rather than [`MAX_SOURCE_BYTES`], and the caller — not
/// this crate — decides which grammars are plausible. Exactly one candidate
/// must parse cleanly; zero or several yield `None`, because reporting a guess
/// or picking among equally valid readings would attach a rule set on no
/// evidence.
pub fn try_detect_content(
    source: &str,
    candidates: &[Language],
) -> Result<Option<Language>, ParseError> {
    validate_source_size(source, MAX_DETECT_BYTES)?;
    Ok(detect_by_parsing(source, candidates))
}

fn detect_by_parsing(source: &str, candidates: &[Language]) -> Option<Language> {
    let mut readable = candidates
        .iter()
        .copied()
        .filter(|&language| try_parse(source, language).is_ok());
    let only = readable.next()?;
    readable.next().is_none().then_some(only)
}

/// Production boundary: refuse oversized bytes, then reject recovery nodes and
/// over-budget trees in one traversal.
pub fn try_parse(code: &str, language: Language) -> Result<Parsed, ParseError> {
    parse_bounded(
        code,
        &language.support_lang(),
        language.name(),
        MAX_SOURCE_BYTES,
        MAX_AST_NODES,
    )
}

/// Checked parse of a caller-registered grammar, held to the same byte bound,
/// node bound, and recovery refusal as [`try_parse`].
pub fn try_parse_with<L: LanguageExt>(
    code: &str,
    language: &L,
    name: &'static str,
) -> Result<AstGrep<StrDoc<L>>, ParseError> {
    parse_bounded(code, language, name, MAX_SOURCE_BYTES, MAX_AST_NODES)
}

fn parse_bounded<L: LanguageExt>(
    code: &str,
    language: &L,
    name: &'static str,
    max_source_bytes: usize,
    max_ast_nodes: usize,
) -> Result<AstGrep<StrDoc<L>>, ParseError> {
    validate_source_size(code, max_source_bytes)?;
    let parsed = AstGrep::try_new(code, language.clone()).map_err(|detail| {
        ParseError::ParserUnavailable {
            language: name,
            detail,
        }
    })?;
    let metrics = inspect_ast(&parsed.root(), Some(max_ast_nodes));
    if metrics.nodes > max_ast_nodes {
        return Err(ParseError::AstTooLarge {
            language: name,
            observed: metrics.nodes,
            limit: max_ast_nodes,
        });
    }
    if metrics.has_syntax_issues {
        return Err(ParseError::InvalidSyntax { language: name });
    }
    Ok(parsed)
}

/// Unchecked parse for diagnostics and tests that intentionally inspect
/// malformed trees. Production call sites use [`try_parse`].
pub fn parse(code: &str, language: Language) -> Parsed {
    parse_with(code, &language.support_lang())
}

/// Unchecked parse of a caller-registered grammar. Production call sites use
/// [`try_parse_with`].
pub fn parse_with<L: LanguageExt>(code: &str, language: &L) -> AstGrep<StrDoc<L>> {
    language.ast_grep(code)
}

fn validate_source_size(source: &str, limit: usize) -> Result<(), ParseError> {
    let actual = source.len();
    (actual <= limit)
        .then_some(())
        .ok_or(ParseError::SourceTooLarge { actual, limit })
}

/// Whether the tree holds an `ERROR` or `MISSING` node. Ask before reporting:
/// on unreadable source, no finding means nothing parsed, not nothing wrong.
pub fn has_syntax_issues<L: LanguageExt>(root: &AstNode<'_, L>) -> bool {
    inspect_ast(root, None).has_syntax_issues
}

/// Deepest branch depth, root counting as 1.
pub fn max_depth<L: LanguageExt>(root: &AstNode<'_, L>) -> usize {
    inspect_ast(root, None).max_depth
}

/// Node count, depth, and recovery state in one heap-backed walk.
///
/// With a node cap, traversal stops at `limit + 1`: enough to prove refusal
/// without letting validation itself go unbounded on a hostile tree.
pub fn inspect_ast<'t, L: LanguageExt>(
    root: &AstNode<'t, L>,
    stop_after_nodes: Option<usize>,
) -> AstMetrics {
    let mut metrics = AstMetrics::default();
    let mut frontier: Vec<(AstNode<'t, L>, usize)> = vec![(root.clone(), 1)];
    while let Some((node, depth)) = frontier.pop() {
        metrics = metrics.including(&node, depth);
        if metrics.nodes > stop_after_nodes.unwrap_or(usize::MAX) {
            break;
        }
        let child_depth = depth.saturating_add(1);
        frontier.extend(node.children().map(|child| (child, child_depth)));
    }
    metrics
}

/// The text of the first direct child whose `kind` equals one of `kinds`, or
/// `None` when no direct child matches. Descendants are not searched, so a
/// caller hunting a nested identifier must walk to that level first.
pub fn child_text_with_kind<L: LanguageExt>(
    node: &AstNode<'_, L>,
    kinds: &[&str],
) -> Option<String> {
    node.children()
        .find(|child| kinds.contains(&child.kind().as_ref()))
        .map(|child| child.text().to_string())
}

/// The name a definition node declares, if a direct child kind in
/// `name_kinds` names it.
///
/// Retained with [`callee_name`] and [`child_text_with_kind`] as the
/// name-resolution surface keel's `lang.rs` calls today; keel issue #556
/// migrates it onto this crate, so removing these would turn that migration
/// into a rewrite.
pub fn definition_name<L: LanguageExt>(
    node: &AstNode<'_, L>,
    name_kinds: &[&str],
) -> Option<String> {
    child_text_with_kind(node, name_kinds)
}

/// The callee a call node names, or `None` when `node` is not a call kind or
/// names none of `name_kinds`. See [`definition_name`] for why this trio is
/// retained.
pub fn callee_name<L: LanguageExt>(
    node: &AstNode<'_, L>,
    call_kinds: &[&str],
    name_kinds: &[&str],
) -> Option<String> {
    call_kinds
        .contains(&node.kind().as_ref())
        .then(|| child_text_with_kind(node, name_kinds))
        .flatten()
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(all(test, feature = "lang-rust"))]
mod tests {
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
            for extension in language.extensions() {
                if let Some((other, _)) = claimed.iter().find(|(ext, _)| ext == extension) {
                    panic!(
                        ".{other} is claimed by both {} and an earlier language",
                        language.name()
                    );
                }
                claimed.push((extension, language.name()));
            }
        }
    }

    #[test]
    fn checked_parse_accepts_valid_rust() {
        let parsed = try_parse("fn f() {}", Language::Rust).expect("valid rust parses");
        assert!(max_depth(&parsed.root()) > 1);
        assert!(!has_syntax_issues(&parsed.root()));
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
            Ok(Some(Language::Rust))
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
        assert_eq!(try_detect_content("fn f() {}", &[]), Ok(None));
        // Two clean readings are ambiguous, so the answer is None rather than a
        // pick. Repeating one candidate is a deterministic way to produce that
        // without depending on a source two grammars happen to agree on.
        assert_eq!(
            try_detect_content("fn f() {}", &[Language::Rust, Language::Rust]),
            Ok(None)
        );
    }

    #[test]
    fn over_budget_tree_refuses_as_ast_too_large() {
        // parse_bounded is the one place the node bound is enforced, so drive it
        // directly with a bound small enough to exceed without a megabyte of
        // source. The walk stops at limit + 1, which is what proves refusal.
        match parse_bounded(
            "fn f() { let x = 1; }",
            &SupportLang::Rust,
            "rust",
            MAX_SOURCE_BYTES,
            2,
        ) {
            Err(ParseError::AstTooLarge {
                observed, limit, ..
            }) => {
                assert_eq!(limit, 2);
                assert!(observed > limit, "observed {observed} must exceed {limit}");
            }
            Err(other) => panic!("expected AstTooLarge, got {other:?}"),
            Ok(_) => panic!("a tree past the bound must refuse"),
        }
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
    fn a_registered_grammar_parses_through_the_bounded_api() {
        let custom = CustomLang::new("rust-as-custom", SupportLang::Rust.get_ts_language());
        let parsed = try_parse_with("fn f() {}", &custom, custom.name()).expect("custom parses");
        assert!(max_depth(&parsed.root()) > 1);
        assert!(!has_syntax_issues(&parsed.root()));
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
            CustomLang::of_path("a/file.custom", std::slice::from_ref(&custom)).map(|l| l.name()),
            Some("rust-as-custom")
        );
        assert_eq!(
            CustomLang::of_path("a/file.cu", std::slice::from_ref(&custom)).map(|l| l.name()),
            Some("rust-as-custom")
        );
        assert!(CustomLang::of_path("a/file.rs", &[custom]).is_none());
    }
}
