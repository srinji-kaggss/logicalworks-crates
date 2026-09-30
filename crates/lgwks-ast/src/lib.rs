#![doc = include_str!("../README.md")]
//! `lgwks_ast` owns the single multi-language AST parser.
//!
//! Code tools that consume this crate, a safety linter and a graph extractor
//! among them, need the same three things: decide a source file's language,
//! select that language's tree-sitter grammar, and walk the resulting syntax
//! tree safely. Before this crate each rebuilt its own language enum,
//! extension table, grammar mapping, and parse loop; that is one concept
//! implemented twice, so it lives here instead.
//!
//! Enforced invariant **INV-AST-ONE-PARSER**: a consumer identifies, selects,
//! and parses through this crate and does not depend on `ast-grep` directly.
//!
//! ## The README is compiled
//!
//! This module documentation is generated from `README.md`, so the usage
//! example above is built and run as a doctest on every `cargo test` rather
//! than being prose that can quietly rot. `cargo test -p lgwks_ast --doc`
//! therefore fails if the example stops compiling or stops asserting what the
//! text above it claims.
//!
//! ## Code observability
//!
//! Code observability here rests on two pieces: structural parsing, and the
//! shared typed-diagnostic derive in [`error`]. [`ParseError`] is built with
//! it, and downstream analysers derive `Display`/`source`/`#[from]` from the
//! same stack instead of each declaring `thiserror`. The crate root re-exports
//! the derive because `#[derive(Error)]` expands to absolute
//! `::thiserror::__private<N>::…` paths resolved in the *consuming* crate: a
//! consumer names this crate `thiserror` (`extern crate lgwks_ast as thiserror;`)
//! and derives normally, with no `thiserror` edge of its own.
//!
//! ## Grammar selection
//!
//! One cargo feature per grammar forwards to `ast-grep-language`. The default
//! enables seven widely-used languages — Rust, Python, TypeScript, JavaScript,
//! Go, Java and Swift — and every remaining grammar ast-grep-language ships
//! (C#, CSS, Dart, Elixir, Haskell, HCL, HTML, JSON, Lua, Markdown, Nix, PHP,
//! Ruby, Solidity, YAML, and the rest) is its own opt-in `lang-*` feature, and
//! `full` enables all 28. A language whose feature is off is not in
//! [`Language::ALL`] and is never returned by [`Language::of_path`], so no
//! consumer pays to compile a grammar it cannot select.
//!
//! Two notes on reading that table. `lang-tsx` is a *selection*, not a separate
//! grammar: ast-grep parses `.tsx` with the TypeScript parser, so it forwards to
//! the same crate as `lang-typescript` and pulls nothing further. And a `#!`
//! line resolves a language where a filename cannot — see
//! [`Language::of_shebang`], which is how an extensionless `bin/deploy` is
//! identified at all.
//!
//! ## Custom languages
//!
//! ast-grep keeps its built-in set small on purpose; its documented extension
//! point for anything else is a caller-registered parser. [`CustomLang`] is
//! that registration: name the language, hand it a `tree-sitter` grammar, and
//! parse it through [`try_parse_with`] under the same bounds and recovery
//! refusal as a built-in. A grammar a consumer here needs should be contributed
//! to `ast-grep-language` upstream and the local registration deleted once it
//! ships. This crate never forks upstream's language tables.
//!
//! ## Bounded parsing
//!
//! A recoverable tree-sitter tree is not proof of valid syntax: recovery emits
//! `ERROR` and `MISSING` nodes. [`try_parse`] therefore refuses before any
//! detector sees the tree: oversized bytes, a parser that cannot produce a
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
//! trial-parses each distinct candidate grammar in full, so the caller names
//! a small candidate set and the probe source is held to [`MAX_DETECT_BYTES`]. The
//! extension-only [`detect`] never parses.
//!
//! ## The README is compiled
//!
//! `README.md` is attached to this crate root as its documentation, so the
//! usage example it opens with is built and run as a doctest on every
//! `cargo test -p lgwks_ast --doc` instead of being prose that can rot. When
//! the example above stops compiling — as it did while `try_parse` was
//! documented as if it were infallible — that doctest is what catches it.

// Lint contract (missing_docs deny, unsafe_code forbid, broken intra-doc
// links deny) comes from the workspace root.

/// Typed diagnostics: [`ParseError`] and the shared error derive.
pub mod error;

/// Spans, severities, and [`diagnostics`](diagnostic::diagnostics): what a tool
/// reports on code, as opposed to what a parse refuses.
pub mod diagnostic;

/// Root re-export required by the `thiserror` derive's absolute expansion path.
///
/// `#[derive(Error)]` expands to `::thiserror::__private<N>::…`, resolved in
/// the *consuming* crate. A consumer with no `thiserror` Cargo edge makes that
/// path resolve here by naming this crate `thiserror`; the glob is what carries
/// the version-suffixed private module.
#[doc(hidden)]
pub use thiserror::*;

use ast_grep_core::Language as CoreLanguage;
use ast_grep_core::matcher::{Pattern, PatternBuilder, PatternError};
use ast_grep_core::tree_sitter::LanguageExt;
pub use ast_grep_core::tree_sitter::{StrDoc, TSLanguage};
pub use ast_grep_core::{AstGrep, Node};
pub use ast_grep_language::SupportLang;

/// The diagnostic types a tool reports with, re-exported at the crate root.
///
/// A tool reporting on code needs [`Diagnostic`], [`Span`], [`Pos`] and
/// [`Severity`] in almost every signature it writes, and reaching into
/// `lgwks_ast::diagnostic::` for all four is a tax on the common case. The
/// module stays public for a caller who prefers the longer path.
pub use diagnostic::{Diagnostic, Pos, Severity, Span};

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
/// detection costs one full parse per distinct candidate grammar, so it is
/// bounded far below [`MAX_SOURCE_BYTES`]; a language probe only needs enough bytes to
/// show one clean reading.
pub const MAX_DETECT_BYTES: usize = 64 * 1024;

/// Longest `#!` line [`Language::of_shebang`] will read (256 bytes).
///
/// A real shebang is a path and an interpreter, and the longest in ordinary use
/// is well under 128 bytes. The bound is what makes the read safe to point at
/// an arbitrary file: a binary whose first "line" is a megabyte of non-newline
/// bytes is refused in constant time rather than scanned.
pub const MAX_SHEBANG_BYTES: usize = 256;

/// Maximum recovery diagnostics retained from one checked parse.
pub const MAX_SYNTAX_DIAGNOSTICS: usize = 32;

/// Define `Language` and its lookup tables from one row per grammar.
///
/// A row is `Variant, doc, feature, name, [extensions], SupportLang`: the enum
/// variant, its doc line, the cargo feature that compiles its grammar, the
/// stable lowercase name reported in findings, the file extensions it answers
/// for without the leading dot, and the `ast-grep-language` twin of the same
/// variant.
///
/// Everything a language owns is generated from that one row (the variant, its
/// slot in `Language::ALL`, and its arm in each of the four lookup tables), so a
/// row cannot be added by halves. The feature gate is applied to all of them
/// together, which is what makes `Language` exhaustive without a wildcard arm
/// when a grammar is compiled out: a disabled language has no variant for a
/// table arm to mention.
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
        #[non_exhaustive]
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
    // `CLang`, not `C`. The workspace forbids `clippy::min_ident_chars`, and a
    // `forbid` cannot be lowered from source: an `#[allow]` here is a hard
    // E0453 rather than a suppression, so the variant name itself has to change.
    // The lint fires on this macro's *input* token, which is why a
    // `#[doc(hidden)]` alias elsewhere would not have helped.
    //
    // The trailing `C` is fine: that is `SupportLang::C`, an external enum's
    // variant reached through a path, and the lint does not visit path segments.
    // Only the leading token had to move. `Language::name()` still reports "c",
    // which is the stable identity callers match on, so nothing that reads a
    // finding is affected; only Rust code naming the variant.
    CLang, "C.", "lang-c", "c", ["c", "h"], C;
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
    #[must_use]
    pub fn of_path(path: &str) -> Option<Self> {
        let extension = extension_of(path)?;
        Self::ALL
            .iter()
            .copied()
            .find(|language| any_extension_matches(language.extensions(), extension))
    }
}

// ── Shebangs ───────────────────────────────────────────────────────────────

/// The interpreter a `#!` line names, reduced to the last path component.
///
/// `#!/usr/bin/env python3`, `#!/usr/bin/python3 -u`, and `#!/opt/py/bin/python`
/// all name a Python interpreter, which is the part a language table can be
/// keyed on. Taking the last component also drops the `-u` and `-Es` flags an
/// interpreter line commonly carries, which are options to that interpreter and
/// not part of its name.
fn interpreter_from_shebang(line: &str) -> Option<&str> {
    let rest = line.strip_prefix("#!")?;
    // A shebang line is `interpreter [optional single arg]`. The argument is
    // separated by whitespace, and everything after the first run of it is an
    // option to the interpreter rather than part of its name.
    let mut words = rest.split_ascii_whitespace();
    let first = words.next()?;
    let named = if is_env(first) {
        // `env` runs a program named by its own arguments, so the interpreter is
        // the first *program* argument. `env -S python3` and `env -Spython3`
        // both spell that the split-S form used by coreutils.
        program_after_env(words)?
    } else {
        first
    };
    let name = named.rsplit('/').next()?;
    // An interpreter with no name in it is not a shebang this can read.
    (!name.is_empty()).then_some(name)
}

/// Whether an interpreter path is `env` rather than a language runtime.
fn is_env(path: &str) -> bool {
    path == "env" || path.ends_with("/env")
}

/// The program `env` was asked to run, from the words after it.
///
/// `env` accepts option flags before the program name, and the split-S form
/// (`-S python3`, `-Spython3`) that lets a shebang carry arguments is written
/// either way in the wild. Both are peeled here so the caller sees the program
/// name and nothing else.
fn program_after_env<'a>(mut words: impl Iterator<Item = &'a str>) -> Option<&'a str> {
    for word in words.by_ref() {
        // A glued split-S argument carries the program on the flag itself:
        // `env -Spython3 -u` runs `python3 -u`.
        if let Some(glued) = word.strip_prefix("-S") {
            if !glued.is_empty() {
                return Some(glued);
            }
            continue;
        }
        // A bare option, with or without a one-letter bundle like `-iu`.
        if word.starts_with('-') {
            continue;
        }
        return Some(word);
    }
    None
}

/// Whether an interpreter name is `name` itself, or a versioned spelling of it.
///
/// Whether an interpreter name is the language `name`, allowing a version.
///
/// A shebang spells the interpreter the way the language names itself, which is
/// not how the file extension is spelled: `python3` is the interpreter and `py`
/// is the extension. So the comparison is against the language's canonical name
/// with a version allowed on the end.
///
/// Two guards keep the looser match honest. The part left over after the name
/// must be a version and nothing else — digits and dots only, and at least one
/// digit — so `python3-config` and `cabal` are not Python. And a name of a
/// single character is never matched this way, so `c` cannot claim the head of
/// an unrelated interpreter.
fn is_versioned_name_of(interpreter: &str, name: &str) -> bool {
    if interpreter.eq_ignore_ascii_case(name) {
        return true;
    }
    if name.len() < MIN_SHEBANG_NAME_BYTES {
        return false;
    }
    let Some(remainder) = interpreter.get(..name.len()) else {
        return false;
    };
    if !remainder.eq_ignore_ascii_case(name) {
        return false;
    }
    interpreter.get(name.len()..).is_some_and(is_version_suffix)
}

/// Shortest language name this will match an interpreter against.
///
/// Two characters, so a one-letter name cannot swallow the head of an unrelated
/// interpreter.
const MIN_SHEBANG_NAME_BYTES: usize = 2;

/// Whether `rest` is a version suffix: at least one digit, and nothing but
/// digits and dots after it.
///
/// One character is enough, because the single most common shebang on a Unix
/// system is `python3` and the single most common versioned interpreter name is
/// exactly that. A bare trailing dot is not a version, since `python.` names
/// nothing.
fn is_version_suffix(rest: &str) -> bool {
    rest.starts_with(|first: char| first.is_ascii_digit())
        && rest
            .chars()
            .all(|part| part.is_ascii_digit() || part == '.')
}

impl Language {
    /// Language named by the `#!` line of `source`, or `None` when it has no
    /// shebang or names an interpreter with no compiled grammar.
    ///
    /// The path-based [`Language::of_path`] cannot route an extensionless
    /// executable, and scripts are routinely shipped exactly that way: a
    /// `bin/deploy` with `#!/usr/bin/env python3` has no extension to read. This
    /// reads the first line and matches the interpreter against the same table
    /// `of_path` uses, so a shebang and a filename resolve to the same
    /// [`Language`] rather than through two tables that can disagree.
    ///
    /// The interpreter is matched against the language's canonical name with a
    /// version allowed, so `python3`, `python3.12` and `python` all resolve to
    /// Python. That is one rule (`is_versioned_name_of`) rather than a second
    /// table of interpreter names to keep in step with this one.
    ///
    /// This is a heuristic and is documented as one: a file whose first line is
    /// `#!/usr/bin/env c` is probably not C, and the ambiguity is the caller's
    /// to resolve through [`try_detect_content`], which parses rather than reads
    /// a name. Where a shebang and an extension disagree, this returns the
    /// shebang; call [`Language::of_path`] when the filename must win.
    ///
    /// Only the first line is examined, and only when it is under
    /// [`MAX_SHEBANG_BYTES`]. A file whose first line is longer than that is
    /// refused rather than scanned, so a binary presented as text costs a
    /// bounded read.
    ///
    /// ```
    /// use lgwks_ast::Language;
    ///
    /// let rust = Language::of_shebang("#!/usr/bin/env rustc\n");
    /// assert_eq!(rust, None, "rustc is not a compiled grammar here");
    /// assert_eq!(
    ///     Language::of_shebang("#!/usr/bin/env not-a-real-interpreter\n"),
    ///     None
    /// );
    /// assert_eq!(Language::of_shebang("#!/bin/sh\n"), None); // no grammar for sh
    /// assert_eq!(Language::of_shebang("fn main() {}\n"), None); // no shebang at all
    /// ```
    #[must_use]
    pub fn of_shebang(source: &str) -> Option<Self> {
        let first = source.split_once('\n').map_or(source, |(line, _)| line);
        if first.len() > MAX_SHEBANG_BYTES {
            return None;
        }
        let interpreter = interpreter_from_shebang(first)?;
        // Matched against the language's canonical name, not its extensions.
        // `python3` is the interpreter and `py` is the extension, so matching
        // an interpreter against extensions would never resolve it: the
        // extension is an abbreviation of the name, not a prefix of it.
        Self::ALL
            .iter()
            .copied()
            .find(|language| is_versioned_name_of(interpreter, language.name()))
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
/// A grammar a consumer here needs should be contributed upstream and the local
/// registration deleted once `ast-grep-language` ships it; keeping it behind
/// this type makes that a localized change, not a fork of upstream's tables.
#[derive(Clone)]
pub struct CustomLang {
    /// Stable lowercase name, reported in findings and diagnostics and used by
    /// the caller to match on. `'static` because the registration outlives any
    /// one parse; a name built at runtime is not registrable.
    name: &'static str,
    /// The grammar itself, cloned into every `StrDoc` this language parses.
    /// `LanguageExt::get_ts_language` hands out a clone, so one registration
    /// serves any number of parses and no parse holds a borrow of it.
    grammar: TSLanguage,
    /// The character standing in for `$` while patterns are compiled: `$`
    /// unless the language treats `$` as an identifier character.
    expando: char,
    /// Extensions answered for, without the leading dot, matched
    /// ASCII-case-insensitively. Empty until [`CustomLang::with_extensions`]
    /// sets them, and never consulted unless a caller passes the registration
    /// to [`CustomLang::of_path`].
    extensions: &'static [&'static str],
}

impl CustomLang {
    /// Register `grammar` under `name`. `$` is assumed valid in the language's
    /// patterns; use [`CustomLang::with_expando`] when it is not.
    #[must_use]
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
    #[must_use]
    pub fn with_expando(mut self, expando: char) -> Self {
        self.expando = expando;
        self
    }

    /// Declare the file extensions this language answers for.
    #[must_use]
    pub fn with_extensions(mut self, extensions: &'static [&'static str]) -> Self {
        self.extensions = extensions;
        self
    }

    /// The lowercase, stable name used in findings and diagnostics.
    #[must_use]
    pub fn name(&self) -> &'static str {
        self.name
    }

    /// Extensions answered for, without the leading dot.
    #[must_use]
    pub fn extensions(&self) -> &'static [&'static str] {
        self.extensions
    }

    /// The first candidate claiming `path`'s extension, case-insensitively.
    /// Registered grammars are selected explicitly; they never join
    /// [`Language::ALL`] or content detection.
    #[must_use]
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
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ParseError {
    /// The source exceeds the byte bound.
    #[error("source is {actual} bytes; parser limit is {limit} bytes")]
    SourceTooLarge {
        /// Observed length in bytes.
        actual: usize,
        /// The applied bound.
        limit: usize,
    },
    /// The selected grammar could not produce a syntax tree.
    #[error("{language} parser could not produce a syntax tree: {detail}")]
    ParserUnavailable {
        /// The language name.
        language: &'static str,
        /// The parser's own detail string.
        detail: String,
    },
    /// The tree carries an `ERROR` or `MISSING` recovery node.
    #[error("{language} parser produced an ERROR or MISSING node")]
    InvalidSyntax {
        /// The language name.
        language: &'static str,
        /// At most [`MAX_SYNTAX_DIAGNOSTICS`] recovery nodes from the bounded walk.
        diagnostics: Vec<SyntaxDiagnostic>,
        /// Whether more recovery nodes were observed than retained.
        diagnostics_truncated: bool,
    },
    /// The tree exceeds the node bound.
    #[error("{language} AST exceeds {limit} nodes (observed at least {observed})")]
    AstTooLarge {
        /// The language name.
        language: &'static str,
        /// Nodes observed up to the bound.
        observed: usize,
        /// The applied bound.
        limit: usize,
    },
}

/// Every recovery node in `tree`, as a located diagnostic.
///
/// This is the form of [`diagnostic::diagnostics`] a caller should reach for.
/// The free function takes the source as an argument, and every span it reports
/// is a byte offset into *that* string — so handing it the wrong one yields
/// positions that are silently off by some amount rather than loudly wrong.
/// Here the source is read out of the tree, which is the only place it can be
/// correct by construction.
///
/// A [`Parsed`] owns its text, so this borrows the tree and nothing else. A
/// clean tree yields an empty `Vec`; a tree carrying recovery nodes yields one
/// [`Diagnostic`] per node, in source order.
///
/// `language` names the grammar in each message, and is the name the caller
/// parsed with — the same string [`try_parse_with`] takes. A caller of the
/// built-in grammars passes `language.name()`; ast-grep's own `Language` trait
/// carries no display name, so this crate cannot recover one from the tree.
///
/// ```
/// use lgwks_ast::Language;
///
/// let source = "fn main() {}\nfn broken( {\n";
/// let language = Language::Rust;
/// let tree = lgwks_ast::parse(source, language);
/// // The source comes from the tree, so no span can resolve wrongly.
/// let found = lgwks_ast::tree_diagnostics("main.rs", &tree, language.name());
/// assert!(!found.is_empty());
/// assert_eq!(found[0].span().start.line, 2);
/// ```
#[must_use]
pub fn tree_diagnostics<L: LanguageExt>(
    path: impl Into<std::path::PathBuf>,
    tree: &Parsed<L>,
    language: &str,
) -> Vec<Diagnostic> {
    diagnostic::diagnostics(path, &tree.root(), tree_source(tree), language)
}

/// How many recovery nodes `tree` carries.
///
/// The count behind [`has_syntax_issues`], which collapses the same question to
/// one sticky bit and cannot say where the damage is.
#[must_use]
pub fn tree_recovery_count<L: LanguageExt>(tree: &Parsed<L>) -> usize {
    diagnostic::recovery_count(&tree.root())
}

/// The text `tree` was parsed from.
///
/// [`Parsed`] is a type alias for ast-grep's own `Root`, whose `doc` field is
/// private, so this reaches the source through a node's public [`Doc`] surface.
/// The tree owns a copy of its text, so the result is never stale and never a
/// re-read from disk.
fn tree_source<L: LanguageExt>(tree: &Parsed<L>) -> &str {
    use ast_grep_core::Doc;
    tree.root().get_doc().get_source()
}

impl ParseError {
    /// This refusal as a located [`Diagnostic`], for a tool that reports every
    /// outcome in one shape.
    ///
    /// Every refusal is a [`Severity::Error`], and none of them is a position
    /// inside the file: a source too large, a grammar that produced no tree, and
    /// a tree carrying recovery nodes are all conditions of the whole file
    /// rather than of a line. The span is therefore zero-width at the end of
    /// `source`, so a renderer underlines nothing rather than pointing at an
    /// invented line.
    ///
    /// ```
    /// use lgwks_ast::{ParseError, Severity};
    ///
    /// let refusal = ParseError::SourceTooLarge { actual: 9, limit: 8 };
    /// let reported = refusal.to_diagnostic("big.rs", "too big");
    /// assert_eq!(reported.severity(), Severity::Error);
    /// assert!(reported.span().is_empty());
    /// ```
    #[must_use]
    pub fn to_diagnostic(&self, path: impl Into<std::path::PathBuf>, source: &str) -> Diagnostic {
        Diagnostic::new(self.to_string(), diagnostic::end_of(source))
            .in_file(path)
            .with_severity(Severity::Error)
    }
}

/// Kind of tree-sitter recovery node reported by a checked parse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SyntaxIssueKind {
    /// Tree-sitter inserted an error node for unparsed source.
    Error,
    /// Tree-sitter inserted a missing token to recover the parse.
    Missing,
}

/// A bounded recovery diagnostic with byte offsets into the original source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct SyntaxDiagnostic {
    /// Whether the recovery node is `ERROR` or `MISSING`.
    pub kind: SyntaxIssueKind,
    /// Inclusive UTF-8 byte offset in the original source.
    pub start_byte: usize,
    /// Exclusive UTF-8 byte offset in the original source.
    pub end_byte: usize,
}

/// Why a bounded AST inspection stopped before visiting the full tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum InspectionStopReason {
    /// The walk observed one node beyond its configured node limit.
    NodeLimitExceeded,
}

/// Complete content-detection result after every distinct candidate was checked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ContentDetection {
    /// No candidate parsed without recovery nodes.
    NoMatch,
    /// Exactly one candidate parsed without recovery nodes.
    Unique(Language),
    /// More than one distinct candidate parsed without recovery nodes.
    Ambiguous,
}

/// Node count, deepest depth, recovery state, and completeness from one traversal.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct AstMetrics {
    /// Nodes visited.
    pub nodes: usize,
    /// Deepest branch depth, root counting as 1.
    pub max_depth: usize,
    /// Whether an `ERROR` or `MISSING` node was seen.
    pub has_syntax_issues: bool,
    /// Whether the whole tree was visited.
    pub complete: bool,
    /// Node limit supplied to the traversal, if any.
    pub node_limit: Option<usize>,
    /// Why the walk stopped early, if inspection is incomplete.
    pub stop_reason: Option<InspectionStopReason>,
}

impl AstMetrics {
    /// Fold one visited `node`, seen at `depth`, into the running metrics.
    ///
    /// Called once per node of a walk, so this is where the three accumulators
    /// are kept monotone: `nodes` saturates instead of wrapping (unreachable at
    /// any real tree size, but the bound is stated rather than assumed),
    /// `max_depth` keeps the deepest branch seen, and `has_syntax_issues` is
    /// sticky: once an `ERROR` or `MISSING` node is seen, no later clean node
    /// may clear it, because the walk has no way to unsee it.
    fn including<L: LanguageExt>(mut self, node: &AstNode<'_, L>, depth: usize) -> Self {
        self.nodes = self.nodes.saturating_add(1);
        self.max_depth = self.max_depth.max(depth);
        self.has_syntax_issues = self.has_syntax_issues || node.is_error() || node.is_missing();
        self
    }
}

/// Identify a language by file extension. `None` means the name does not claim
/// the file, so report unscanned, never clean.
///
/// Content sniffing is deliberately not part of this call: it costs one full
/// parse per distinct grammar. A caller that needs it opts in with
/// [`try_detect_content`] and names the few grammars it expects, rather than
/// trial-parsing every language this build carries.
#[must_use]
pub fn detect(path: &str) -> Option<Language> {
    Language::of_path(path)
}

/// Identify `source` by trial-parsing `candidates`, the opt-in content path.
///
/// Each distinct candidate is parsed once, so `source` is held to
/// [`MAX_DETECT_BYTES`] rather than [`MAX_SOURCE_BYTES`], and the caller, not
/// this crate, decides which grammars are plausible. The result distinguishes
/// no match, one clean candidate, and multiple clean candidates. A
/// parser-unavailable or node-budget refusal returns `Err`: an uninspected
/// required candidate cannot be treated as evidence against it.
pub fn try_detect_content(
    source: &str,
    candidates: &[Language],
) -> Result<ContentDetection, ParseError> {
    validate_source_size(source, MAX_DETECT_BYTES)?;
    detect_by_parsing(source, candidates)
}

/// Classify `source` across all distinct candidates, returning parser failures
/// because they leave the candidate set incompletely inspected.
///
/// `source` is assumed already size-checked by the caller: [`try_detect_content`]
/// is the only one, and it applies [`MAX_DETECT_BYTES`] before reaching here.
/// "Several clean readings" is a refusal rather than a tie-break: picking among
/// grammars that all accept the source would attach a rule set on no evidence,
/// which is the same defect as reporting a guess. Duplicate candidates are
/// discarded against the finite compiled grammar table before parsing.
fn detect_by_parsing(
    source: &str,
    candidates: &[Language],
) -> Result<ContentDetection, ParseError> {
    detect_candidates(candidates, |language| {
        try_parse(source, language).map(|_| ())
    })
}

/// Parse each distinct compiled candidate once and refuse incomplete inspection.
fn detect_candidates(
    candidates: &[Language],
    mut parse_candidate: impl FnMut(Language) -> Result<(), ParseError>,
) -> Result<ContentDetection, ParseError> {
    let mut seen = vec![false; Language::ALL.len()];
    let mut unique_reading = None;
    let mut ambiguous = false;
    let mut incomplete = None;

    for &language in candidates {
        let Some(index) = Language::ALL
            .iter()
            .position(|&compiled| compiled == language)
        else {
            keep_first_incomplete(
                language,
                ParseError::ParserUnavailable {
                    language: language.name(),
                    detail: "candidate grammar is not present in this build".to_owned(),
                },
                &mut incomplete,
            );
            continue;
        };
        if std::mem::replace(&mut seen[index], true) {
            continue;
        }
        match parse_candidate(language) {
            Ok(()) => {
                if unique_reading.replace(language).is_some() {
                    ambiguous = true;
                }
            }
            Err(ParseError::InvalidSyntax { .. }) => {}
            Err(error) => keep_first_incomplete(language, error, &mut incomplete),
        }
    }

    if let Some((_, error)) = incomplete {
        return Err(error);
    }
    if ambiguous {
        Ok(ContentDetection::Ambiguous)
    } else if let Some(language) = unique_reading {
        Ok(ContentDetection::Unique(language))
    } else {
        Ok(ContentDetection::NoMatch)
    }
}

/// Keep the lexically first failed candidate so permutations report the same refusal.
fn keep_first_incomplete(
    language: Language,
    error: ParseError,
    incomplete: &mut Option<(Language, ParseError)>,
) {
    if incomplete
        .as_ref()
        .is_none_or(|&(recorded, _)| language.name() < recorded.name())
    {
        *incomplete = Some((language, error));
    }
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

/// The one body behind [`try_parse`] and [`try_parse_with`]: byte bound, then
/// tree, then node bound, then recovery refusal.
///
/// The bounds are parameters rather than constants read in place so both entry
/// points state the policy they apply at the call site, and so a later caller
/// with a different budget reuses this ordering instead of restating it. The
/// order is the point: the byte check runs before the parser allocates, the
/// node check before any walk of the tree, and both before any tree reaches a
/// caller.
///
/// `language` is borrowed and cloned into the `StrDoc` because a grammar may be
/// a registered [`CustomLang`] or a built-in [`Language`]; `name` is the stable
/// name the error variants carry, since they cannot hold the grammar itself.
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
    let (metrics, _, diagnostics, diagnostics_truncated) =
        inspect_ast_with_pending(&parsed.root(), Some(max_ast_nodes));
    if !metrics.complete {
        return Err(ParseError::AstTooLarge {
            language: name,
            observed: metrics.nodes,
            limit: max_ast_nodes,
        });
    }
    if metrics.has_syntax_issues {
        return Err(ParseError::InvalidSyntax {
            language: name,
            diagnostics,
            diagnostics_truncated,
        });
    }
    Ok(parsed)
}

/// Unchecked parse for diagnostics and tests that intentionally inspect
/// malformed trees. Production call sites use [`try_parse`].
#[must_use]
pub fn parse(code: &str, language: Language) -> Parsed {
    parse_with(code, &language.support_lang())
}

/// Unchecked parse of a caller-registered grammar. Production call sites use
/// [`try_parse_with`].
pub fn parse_with<L: LanguageExt>(code: &str, language: &L) -> AstGrep<StrDoc<L>> {
    language.ast_grep(code)
}

/// Refuse `source` over `limit` bytes, naming both numbers in the refusal.
///
/// The measure is bytes, not characters: the bound exists to keep parse work
/// linear in input, and what the parser is handed is bytes. `actual` travels
/// into the error rather than being dropped, so a caller can report how far
/// over the input was without measuring it a second time.
fn validate_source_size(source: &str, limit: usize) -> Result<(), ParseError> {
    let actual = source.len();
    (actual <= limit)
        .then_some(())
        .ok_or(ParseError::SourceTooLarge { actual, limit })
}

/// Whether the tree holds an `ERROR` or `MISSING` node. Ask before reporting:
/// on unreadable source, no finding means nothing parsed, not nothing wrong.
#[must_use]
pub fn has_syntax_issues<L: LanguageExt>(root: &AstNode<'_, L>) -> bool {
    inspect_ast(root, None).has_syntax_issues
}

/// Deepest branch depth, root counting as 1.
#[must_use]
pub fn max_depth<L: LanguageExt>(root: &AstNode<'_, L>) -> usize {
    inspect_ast(root, None).max_depth
}

/// Node count, depth, recovery state, and traversal completeness in one walk.
///
/// With a node cap, traversal stops at `limit + 1`: enough to prove refusal
/// without letting validation itself go unbounded on a hostile tree. The
/// returned metrics preserve that cap and identify incomplete inspection;
/// `has_syntax_issues == false` is not evidence of a clean tree unless
/// `complete` is also true.
#[must_use]
pub fn inspect_ast<'t, L: LanguageExt>(
    root: &AstNode<'t, L>,
    stop_after_nodes: Option<usize>,
) -> AstMetrics {
    inspect_ast_with_pending(root, stop_after_nodes).0
}

/// Inspect without retaining all siblings in one pending vector.
///
/// Each frame owns one node, its next child index, and depth: at most one
/// frame per active ancestor. The bounded path therefore allocates according
/// to tree depth, not the root's fan-out. `peak_frames` is kept private as a
/// directly asserted resource invariant rather than exported as a public
/// metric whose callers might mistake it for a configured limit.
fn inspect_ast_with_pending<'t, L: LanguageExt>(
    root: &AstNode<'t, L>,
    stop_after_nodes: Option<usize>,
) -> (AstMetrics, usize, Vec<SyntaxDiagnostic>, bool) {
    let mut metrics = AstMetrics {
        complete: true,
        node_limit: stop_after_nodes,
        ..AstMetrics::default()
    };
    let mut diagnostics = Vec::new();
    let mut diagnostics_truncated = false;
    // Frame = (node, depth, remaining child index). Visiting the next lower
    // index preserves the former stack walk's reverse-sibling order without
    // enqueuing the sibling frontier.
    let mut frames = vec![(root.clone(), 1_usize, root.children().len())];
    let mut peak_frames = 1;
    metrics = metrics.including(root, 1);
    record_syntax_diagnostic(root, &mut diagnostics, &mut diagnostics_truncated);
    if stop_after_nodes.is_some_and(|limit| metrics.nodes > limit) {
        mark_inspection_incomplete(&mut metrics);
        return (metrics, peak_frames, diagnostics, diagnostics_truncated);
    }
    while let Some(frame) = frames.last_mut() {
        let Some(child_index) = frame.2.checked_sub(1) else {
            frames.pop();
            continue;
        };
        frame.2 = child_index;
        let next_child = frame.0.child(child_index);
        let Some(child) = next_child else {
            continue;
        };
        let child_depth = frame.1.saturating_add(1);
        metrics = metrics.including(&child, child_depth);
        record_syntax_diagnostic(&child, &mut diagnostics, &mut diagnostics_truncated);
        if stop_after_nodes.is_some_and(|limit| metrics.nodes > limit) {
            mark_inspection_incomplete(&mut metrics);
            break;
        }
        let child_count = child.children().len();
        frames.push((child, child_depth, child_count));
        peak_frames = peak_frames.max(frames.len());
    }
    diagnostics.sort_unstable_by_key(syntax_position);
    (metrics, peak_frames, diagnostics, diagnostics_truncated)
}

/// Mark a walk as partial after it observes the node beyond its configured cap.
fn mark_inspection_incomplete(metrics: &mut AstMetrics) {
    metrics.complete = false;
    metrics.stop_reason = Some(InspectionStopReason::NodeLimitExceeded);
}

/// Retain one recovery-node span until the public diagnostic ceiling is reached.
fn record_syntax_diagnostic<L: LanguageExt>(
    node: &AstNode<'_, L>,
    diagnostics: &mut Vec<SyntaxDiagnostic>,
    diagnostics_truncated: &mut bool,
) {
    let kind = if node.is_error() {
        Some(SyntaxIssueKind::Error)
    } else if node.is_missing() {
        Some(SyntaxIssueKind::Missing)
    } else {
        None
    };
    let Some(kind) = kind else {
        return;
    };
    let span = node.range();
    push_syntax_diagnostic(
        SyntaxDiagnostic {
            kind,
            start_byte: span.start,
            end_byte: span.end,
        },
        diagnostics,
        diagnostics_truncated,
    );
}

/// Where a diagnostic sits in the source, for keeping and ordering them.
fn syntax_position(diagnostic: &SyntaxDiagnostic) -> (usize, usize) {
    (diagnostic.start_byte, diagnostic.end_byte)
}

/// Retain a diagnostic under the fixed per-parse ceiling, keeping the earliest.
///
/// The walk visits siblings in reverse order, so "the first
/// [`MAX_SYNTAX_DIAGNOSTICS`] seen" would be the last ones in the file, and
/// the error a reader needs first — where the source stopped parsing — would
/// be the one dropped. At the ceiling a new diagnostic replaces the latest one
/// retained when it starts earlier; the retained set is always the earliest in
/// source order, in constant memory.
fn push_syntax_diagnostic(
    diagnostic: SyntaxDiagnostic,
    diagnostics: &mut Vec<SyntaxDiagnostic>,
    diagnostics_truncated: &mut bool,
) {
    if diagnostics.len() < MAX_SYNTAX_DIAGNOSTICS {
        diagnostics.push(diagnostic);
        return;
    }
    *diagnostics_truncated = true;
    let latest = diagnostics
        .iter_mut()
        .max_by_key(|retained| syntax_position(retained));
    if let Some(latest) = latest
        && syntax_position(&diagnostic) < syntax_position(latest)
    {
        *latest = diagnostic;
    }
}

/// The owned text of the first direct child whose `kind` equals one of `kinds`,
/// or `None` when no direct child matches. Descendants are not searched, so a
/// caller hunting a nested identifier must walk to that level first. The text
/// is owned because the node's source borrow is tied to the parsed tree.
#[must_use]
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
/// With [`callee_name`] and [`child_text_with_kind`], the name-resolution
/// surface. These three are the only way to read a declared name without
/// writing a tree walk by hand, so they stay on the surface deliberately. That
/// is a judgement about this crate's own API, not a claim about callers: no
/// crate in this workspace depends on `lgwks_ast` today, and these three carry
/// no callers outside this crate's tests.
#[must_use]
pub fn definition_name<L: LanguageExt>(
    node: &AstNode<'_, L>,
    name_kinds: &[&str],
) -> Option<String> {
    child_text_with_kind(node, name_kinds)
}

/// The callee a call node names, or `None` when `node` is not a call kind or
/// names none of `name_kinds`. See [`definition_name`] for why this trio is
/// retained.
#[must_use]
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
    fn a_small_node_budget_does_not_retain_a_wide_sibling_frontier() {
        let source = (0..4_096)
            .map(|index| format!("fn f{index}() {{}}\n"))
            .collect::<String>();
        let parsed = parse(&source, Language::Rust);
        assert_eq!(
            parsed.root().children().len(),
            4_096,
            "the fixture presents 4,096 immediate siblings to the traversal"
        );

        let (metrics, peak_frames, _, _) = inspect_ast_with_pending(&parsed.root(), Some(1));

        assert_eq!(metrics.nodes, 2, "one node beyond the cap proves refusal");
        assert_eq!(
            inspect_ast(&parsed.root(), Some(1)).nodes,
            2,
            "the public inspector preserves the budget's overflow witness"
        );
        assert_eq!(
            peak_frames, 1,
            "the overflow witness is counted without enqueuing its siblings"
        );
    }

    #[test]
    fn a_node_budget_preserves_the_existing_reverse_sibling_order() {
        let parsed = parse("fn okay() {}\n@\n", Language::Rust);
        let root = parsed.root();
        assert!(
            root.children().last().is_some_and(|node| node.is_error()),
            "the fixture puts a recovery node in the last root-child position"
        );

        let metrics = inspect_ast(&root, Some(1));

        assert_eq!(metrics.nodes, 2, "the overflow witness is counted");
        assert!(
            metrics.has_syntax_issues,
            "reverse-sibling traversal encounters the last root child first"
        );
    }

    #[test]
    fn a_completed_walk_retains_one_frame_per_active_ancestor() {
        // The other half of INV-AST-1, and the half the budget test above
        // cannot reach. That test stops at the cap before descending, so it
        // proves the early break is cheap; it says nothing about the steady
        // state, which is the path every real walk takes (the production cap is
        // 2,000,000 nodes and is never hit). This one runs to completion.
        //
        // A deep, narrow tree: each level has one child, so the number of active
        // ancestors is the depth, and the frames retained must be that depth
        // rather than the total node count.
        let depth = 200;
        let source = format!(
            "{}fn f() {{}}{}",
            "fn f() {".repeat(depth),
            "}".repeat(depth)
        );
        let parsed = parse(&source, Language::Rust);
        let (metrics, peak_frames, _, _) = inspect_ast_with_pending(&parsed.root(), None);

        assert!(
            metrics.max_depth > 50,
            "the fixture is {} deep, so the walk really does descend",
            metrics.max_depth
        );
        assert_eq!(
            peak_frames, metrics.max_depth,
            "retained {peak_frames} frames for a tree {} deep; the walk is not \
             retaining exactly the active ancestors",
            metrics.max_depth
        );
        assert!(
            metrics.nodes > peak_frames.saturating_mul(4),
            "retained {peak_frames} frames for a tree of {} nodes, which is a \
             sibling frontier rather than a depth",
            metrics.nodes
        );
    }

    #[test]
    fn inspection_metrics_name_complete_exact_and_over_limit_walks() {
        let parsed = parse("fn a() {}\nfn b() {}", Language::Rust);
        let root = parsed.root();
        let full = inspect_ast(&root, None);
        assert!(
            full.complete,
            "an unlimited traversal inspects the whole tree"
        );
        assert_eq!(
            full.node_limit, None,
            "unlimited inspection has no applied cap"
        );
        assert_eq!(
            full.stop_reason, None,
            "complete inspection has no stop reason"
        );

        let exact = inspect_ast(&root, Some(full.nodes));
        assert!(
            exact.complete,
            "visiting exactly the cap is a complete traversal"
        );
        assert_eq!(
            exact.node_limit,
            Some(full.nodes),
            "the applied cap is retained"
        );

        let over = inspect_ast(&root, Some(full.nodes - 1));
        assert!(
            !over.complete,
            "a cap below the tree size marks partial metrics"
        );
        assert_eq!(over.nodes, full.nodes, "the overflow witness is counted");
        assert_eq!(
            over.node_limit,
            Some(full.nodes - 1),
            "partial metrics preserve the configured limit"
        );
        assert_eq!(
            over.stop_reason,
            Some(InspectionStopReason::NodeLimitExceeded),
            "partial metrics identify the stop reason"
        );

        let zero = inspect_ast(&root, Some(0));
        assert!(
            !zero.complete,
            "the root is the limit-plus-one witness at zero cap"
        );
        assert_eq!(zero.nodes, 1, "zero cap still records the root observation");
    }

    #[test]
    fn recovery_beyond_the_visit_cap_is_not_reported_as_clean() {
        let parsed = parse("@\nfn a() {}\nfn b() {}", Language::Rust);
        let partial = inspect_ast(&parsed.root(), Some(1));
        assert!(
            !partial.complete,
            "the small cap stops before the early recovery node"
        );
        assert!(
            !partial.has_syntax_issues,
            "unvisited nodes are not called clean or faulty"
        );

        let complete = inspect_ast(&parsed.root(), None);
        assert!(
            complete.complete,
            "the uncapped walk reaches the whole tree"
        );
        assert!(
            complete.has_syntax_issues,
            "the full walk observes the recovery node"
        );
    }

    #[test]
    fn syntax_diagnostics_distinguish_recovery_kinds_and_use_source_byte_ranges()
    -> Result<(), Box<dyn std::error::Error>> {
        let source = "fn main() {\n    let café = ;\n}\n";
        let invalid = try_parse(source, Language::Rust);
        let diagnostics = match invalid {
            Err(ParseError::InvalidSyntax { diagnostics, .. }) => diagnostics,
            other => {
                return Err(format!("expected syntax diagnostics, got {:?}", other.err()).into());
            }
        };
        let error = diagnostics
            .iter()
            .find(|diagnostic| diagnostic.kind == SyntaxIssueKind::Error)
            .ok_or_else(|| std::io::Error::other("the malformed expression has no ERROR node"))?;
        assert!(
            source.is_char_boundary(error.start_byte),
            "start byte is a UTF-8 boundary"
        );
        assert!(
            source.is_char_boundary(error.end_byte),
            "end byte is a UTF-8 boundary"
        );
        assert!(
            error.start_byte <= error.end_byte,
            "diagnostic span is ordered"
        );
        assert!(
            source.get(error.start_byte..error.end_byte).is_some(),
            "span slices the source"
        );
        assert!(
            source[..error.start_byte].contains('é'),
            "span follows the multibyte prefix"
        );

        let missing = try_parse("fn f() { let x = 1 }", Language::Rust);
        assert!(
            matches!(
                missing,
                Err(ParseError::InvalidSyntax { ref diagnostics, .. })
                    if diagnostics.iter().any(|diagnostic| diagnostic.kind == SyntaxIssueKind::Missing)
            ),
            "an omitted let semicolon is reported as MISSING"
        );
        Ok(())
    }

    #[test]
    fn a_truncated_syntax_report_keeps_the_earliest_errors_in_source_order()
    -> Result<(), Box<dyn std::error::Error>> {
        // Far more broken items than the ceiling, one per line, so the error
        // on the first line is the one a reader needs and the walk, which runs
        // siblings in reverse, meets it last.
        let source = "fn broken( {\n".repeat(MAX_SYNTAX_DIAGNOSTICS * 3);
        let (diagnostics, truncated) = match try_parse(&source, Language::Rust) {
            Err(ParseError::InvalidSyntax {
                diagnostics,
                diagnostics_truncated,
                ..
            }) => (diagnostics, diagnostics_truncated),
            other => {
                return Err(format!("expected InvalidSyntax, got {:?}", other.err()).into());
            }
        };
        assert!(
            truncated,
            "more recovery nodes than the ceiling were observed"
        );
        assert_eq!(diagnostics.len(), MAX_SYNTAX_DIAGNOSTICS);
        let first = diagnostics
            .first()
            .ok_or("a truncated report is not empty")?;
        assert!(
            first.start_byte < "fn broken( {\n".len(),
            "the first retained error is on the first line, not {first:?}"
        );
        assert!(
            diagnostics.windows(2).all(|pair| matches!(
                pair,
                [earlier, later] if syntax_position(earlier) <= syntax_position(later)
            )),
            "the retained errors are in source order"
        );
        Ok(())
    }

    #[test]
    fn the_refusal_and_the_report_count_the_same_recovery_nodes()
    -> Result<(), Box<dyn std::error::Error>> {
        // Under the ceiling, the refusal's diagnostics and the rendered report
        // are two views of one walk's node set; they must not disagree.
        let source = "fn main() {\n    let x = ;\n}\nfn broken( {\n";
        let refused = match try_parse(source, Language::Rust) {
            Err(ParseError::InvalidSyntax {
                diagnostics,
                diagnostics_truncated: false,
                ..
            }) => diagnostics,
            other => {
                return Err(
                    format!("expected an untruncated refusal, got {:?}", other.err()).into(),
                );
            }
        };
        let tree = parse(source, Language::Rust);
        assert_eq!(tree_recovery_count(&tree), refused.len());
        assert_eq!(
            tree_diagnostics("main.rs", &tree, Language::Rust.name()).len(),
            refused.len()
        );
        Ok(())
    }

    #[test]
    fn syntax_diagnostics_stop_at_the_declared_bound() {
        let mut diagnostics = Vec::new();
        let mut truncated = false;
        let diagnostic = SyntaxDiagnostic {
            kind: SyntaxIssueKind::Error,
            start_byte: 0,
            end_byte: 0,
        };
        for _ in 0..MAX_SYNTAX_DIAGNOSTICS + 1 {
            push_syntax_diagnostic(diagnostic, &mut diagnostics, &mut truncated);
        }
        assert_eq!(
            diagnostics.len(),
            MAX_SYNTAX_DIAGNOSTICS,
            "retained diagnostic memory is bounded by the public ceiling"
        );
        assert!(truncated, "omitted diagnostics are reported as truncated");
    }

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
            MAX_SOURCE_BYTES,
            2,
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
}

// ── Feature-matrix acceptance ───────────────────────────────────────────────
// Compiled unconditionally so `cargo test --no-default-features` asserts
// something instead of passing vacuously. What a build with a grammar must
// prove is not that one path lookup returns `Some` but that the grammar it
// compiled reads its own language: a build with no grammar claims no path at
// all, and every build claims exactly the extensions its compiled table
// declares.
//
// The `lang-*` features are independent by design, so no assertion here may
// name a language the build might not have compiled: `Language::Rust` does not
// exist without `lang-rust`, and a Python-only build is non-empty while
// correctly refusing `.rs`.

#[cfg(test)]
mod feature_matrix_tests {
    use super::*;

    /// Paths every build answers from its own compiled table.
    ///
    /// Not one path per declared extension: [`Language::of_path`] is driven by
    /// the table itself, so one path per *shape* is enough here — extensions
    /// from the default set, ones only a standalone `lang-*` feature declares,
    /// an extension no grammar declares, and a path with no extension at all.
    /// The extension-by-extension check is
    /// `every_compiled_language_resolves_its_own_extensions`.
    const PROBES: &[&str] = &[
        "src/lib.rs",
        "src/main.py",
        "src/app.go",
        "src/app.md",
        "src/app.c",
        "src/probe.lgwks-unclaimed",
        "noextension",
    ];

    /// Whether any compiled language declares `extension`.
    ///
    /// This reads the table from the test side so the assertions below compare
    /// the lookup against the *compiled* set rather than against a path someone
    /// assumed would be present.
    fn table_claims(extension: &str) -> bool {
        Language::ALL
            .iter()
            .any(|language| any_extension_matches(language.extensions(), extension))
    }

    /// A path is claimed exactly when a compiled grammar declares its extension.
    ///
    /// Its predecessor asserted `Language::of_path("src/lib.rs").is_some()`
    /// whenever [`Language::ALL`] was non-empty. That implication does not hold:
    /// `lang-rust` is independent of every other grammar feature, so a
    /// standalone Python build is non-empty and correctly does not claim `.rs`.
    ///
    /// Read what this loop can and cannot tell you. It compares [`Language::of_path`]
    /// against the same compiled table `of_path` is built from — `extension_of`
    /// and `any_extension_matches` over [`Language::ALL`] — so it is a
    /// consistency check between the aggregate lookup and the per-language
    /// extension lists, not independent evidence that either is right. It
    /// cannot fail while those two agree, and it asserts only `is_some()`, so it
    /// says nothing about *which* language a path resolves to.
    ///
    /// The regression the report described is pinned by concrete assertions
    /// instead: the two `rust_paths_resolve_*` tests below, which name
    /// `Some(Language::Rust)` and `None` outright, the [`Language::ALL`]
    /// emptiness block at the end of this test, and
    /// `every_compiled_language_resolves_its_own_extensions`, which pairs each
    /// compiled language with its own declared extension.
    #[test]
    fn empty_build_selects_nothing() {
        for &probe in PROBES {
            let expected = extension_of(probe).is_some_and(table_claims);
            assert_eq!(
                Language::of_path(probe).is_some(),
                expected,
                "{probe}: claimed exactly when a compiled grammar declares its extension"
            );
            assert_eq!(
                detect(probe).is_some(),
                expected,
                "{probe}: `detect` is the same lookup under a second name"
            );
        }
        if Language::ALL.is_empty() {
            assert!(
                Language::of_path("src/lib.rs").is_none(),
                "a build with no grammar compiled must not claim .rs"
            );
            assert!(
                Language::of_path("src/main.py").is_none(),
                "a build with no grammar compiled must not claim .py"
            );
        }
    }

    /// `.rs` resolves exactly when the Rust grammar was compiled.
    ///
    /// [`Language::Rust`] does not exist without `lang-rust`, so the two
    /// directions are two tests rather than two branches of one; both assert
    /// the lookup rather than inferring it from a non-empty [`Language::ALL`].
    #[cfg(feature = "lang-rust")]
    #[test]
    fn rust_paths_resolve_with_the_rust_grammar() {
        assert_eq!(
            Language::of_path("src/lib.rs"),
            Some(Language::Rust),
            "a build with the Rust grammar must claim .rs"
        );
        assert_eq!(
            detect("src/lib.rs"),
            Some(Language::Rust),
            "detect is of_path under a second name"
        );
    }

    /// The other direction: no Rust grammar, no `.rs`.
    #[cfg(not(feature = "lang-rust"))]
    #[test]
    fn rust_paths_resolve_to_nothing_without_the_rust_grammar() {
        assert!(
            Language::of_path("src/lib.rs").is_none(),
            "a build without the Rust grammar must not claim .rs, however many other grammars it compiled"
        );
        assert!(
            detect("src/lib.rs").is_none(),
            "detect is of_path under a second name"
        );
    }

    /// Every compiled grammar resolves each extension it declares.
    ///
    /// The module above asserts this too, but only for builds that compiled
    /// `lang-rust`; a standalone grammar build has no counterpart there, and
    /// the lookup is exactly what such a build must not get wrong.
    #[test]
    fn every_compiled_language_resolves_its_own_extensions() {
        for &language in Language::ALL {
            for &extension in language.extensions() {
                let path = format!("dir/probe.{extension}");
                assert_eq!(
                    Language::of_path(&path),
                    Some(language),
                    "{}: {path} must resolve to its own language",
                    language.name()
                );
            }
        }
    }

    /// Every compiled grammar applies the public byte bound before parsing.
    ///
    /// The bound is checked ahead of the parser, so it holds for any grammar;
    /// this pins that it is a property of the call rather than of Rust, which
    /// is the only grammar the `lang-rust`-gated tests exercise it with.
    #[test]
    fn every_compiled_language_refuses_oversized_source() {
        let oversized = "x".repeat(MAX_SOURCE_BYTES.saturating_add(1));
        for &language in Language::ALL {
            assert!(
                matches!(
                    try_parse(&oversized, language),
                    Err(ParseError::SourceTooLarge { .. })
                ),
                "{}: source past MAX_SOURCE_BYTES must be refused before parsing",
                language.name()
            );
        }
    }

    /// How many grammars the manifest declares, one `lang-*` feature each.
    ///
    /// The fixture table below is the acceptance matrix for these, so the count
    /// is stated once here and checked against the table rather than repeated
    /// per row.
    const DECLARED_GRAMMARS: usize = 28;

    /// What one declared grammar must accept and refuse.
    struct GrammarFixture {
        /// The [`Language::name`] this row covers.
        language: &'static str,
        /// Source the compiled grammar must accept without a recovery node.
        valid: &'static str,
        /// Source the compiled grammar must refuse as `InvalidSyntax`.
        malformed: &'static str,
    }

    /// One valid and one malformed fixture per declared grammar.
    ///
    /// The malformed half is the half that proves something. tree-sitter
    /// recovers from nearly everything, so a grammar that "parses" a fixture
    /// may only be demonstrating recovery; each row's malformed source is a
    /// construct the grammar cannot complete — an unterminated block, string,
    /// or collection — which a working grammar reports through an `ERROR` or
    /// `MISSING` node and [`try_parse`] therefore refuses.
    ///
    /// Rows are keyed on the stable [`Language::name`] rather than on a
    /// variant, because a row must be nameable in a build whose feature did not
    /// compile the variant: this table is complete in every configuration, and
    /// `Language::ALL` decides which rows a given build runs.
    const FIXTURES: &[GrammarFixture] = &[
        GrammarFixture {
            language: "bash",
            valid: "echo hello\n",
            malformed: "echo \"unterminated\n",
        },
        GrammarFixture {
            language: "c",
            valid: "int main(void) { return 0; }\n",
            malformed: "int main(void) { return 0;\n",
        },
        GrammarFixture {
            language: "cpp",
            valid: "int main() { return 0; }\n",
            malformed: "int main() { return 0;\n",
        },
        GrammarFixture {
            language: "csharp",
            valid: "class A { }\n",
            malformed: "class A {\n",
        },
        GrammarFixture {
            language: "css",
            valid: "a { color: red; }\n",
            malformed: "a { color: red;\n",
        },
        GrammarFixture {
            language: "dart",
            valid: "void main() {}\n",
            malformed: "void main() {\n",
        },
        GrammarFixture {
            language: "elixir",
            valid: "defmodule A do\n  def f, do: 1\nend\n",
            malformed: "defmodule A do\n  def f, do: 1\n",
        },
        GrammarFixture {
            language: "go",
            valid: "package main\n\nfunc main() {}\n",
            malformed: "package main\n\nfunc main() {\n",
        },
        GrammarFixture {
            language: "haskell",
            valid: "main = putStrLn \"hi\"\n",
            malformed: "main = putStrLn \"hi\n",
        },
        GrammarFixture {
            language: "hcl",
            valid: "resource \"a\" \"b\" {\n}\n",
            malformed: "resource \"a\" \"b\" {\n",
        },
        GrammarFixture {
            language: "html",
            valid: "<!DOCTYPE html>\n<html><body><p>hi</p></body></html>\n",
            malformed: "<html><body><p>hi\n",
        },
        GrammarFixture {
            language: "java",
            valid: "class A {}\n",
            malformed: "class A {\n",
        },
        GrammarFixture {
            language: "javascript",
            valid: "const a = 1;\n",
            malformed: "function f() {\n",
        },
        GrammarFixture {
            language: "json",
            valid: "{\"a\": 1}\n",
            malformed: "{\"a\": 1\n",
        },
        GrammarFixture {
            language: "kotlin",
            valid: "fun main() {}\n",
            malformed: "fun main() {\n",
        },
        GrammarFixture {
            language: "lua",
            valid: "local a = 1\n",
            malformed: "function f(\n",
        },
        // Markdown's refusal surface is its table scanner:
        // `pipe_table_delimiter_row` in the block grammar requires a `|` after
        // every delimiter cell, so a header row followed by `|---` cannot
        // complete and carries a `MISSING` node. The source is otherwise valid
        // GFM, which is the point of pinning it — the fixture records what
        // *this* compiled grammar refuses, so a grammar bump that starts
        // accepting it fails this row rather than passing quietly. The valid
        // row carries a table written the way that scanner accepts it.
        GrammarFixture {
            language: "markdown",
            valid: "# Title\n\n| a | b |\n|---|---|\n| 1 | 2 |\n",
            malformed: "| a |\n|---\n| b |\n",
        },
        GrammarFixture {
            language: "nix",
            valid: "{ pkgs }: pkgs.hello\n",
            malformed: "let a = 1;\n",
        },
        GrammarFixture {
            language: "php",
            valid: "<?php echo \"hi\";\n",
            malformed: "<?php function f() {\n",
        },
        GrammarFixture {
            language: "python",
            valid: "def f():\n    return 1\n",
            malformed: "def f(:\n    return 1\n",
        },
        GrammarFixture {
            language: "ruby",
            valid: "def f\n  1\nend\n",
            malformed: "def f\n  1\n",
        },
        GrammarFixture {
            language: "rust",
            valid: "fn main() {}\n",
            malformed: "fn main( {\n",
        },
        GrammarFixture {
            language: "scala",
            valid: "object A { def f = 1 }\n",
            malformed: "object A { def f = 1\n",
        },
        GrammarFixture {
            language: "solidity",
            valid: "contract A {}\n",
            malformed: "contract A {\n",
        },
        GrammarFixture {
            language: "swift",
            valid: "func f() {}\n",
            malformed: "func f() {\n",
        },
        GrammarFixture {
            language: "tsx",
            valid: "const A = () => <div />;\n",
            malformed: "function f() {\n",
        },
        GrammarFixture {
            language: "typescript",
            valid: "const a: number = 1;\n",
            malformed: "function f() {\n",
        },
        GrammarFixture {
            language: "yaml",
            valid: "a: 1\n",
            malformed: "a: [1, 2\n",
        },
    ];

    /// Every compiled grammar reads its own language, and refuses a source that
    /// is not one.
    ///
    /// A path lookup returning `Some` only proves a table row exists; this is
    /// the test that proves the grammar behind the row works. Both directions
    /// go through the public checked API, so a "valid" fixture that the grammar
    /// only *recovers* into a tree fails, and so does a malformed fixture the
    /// grammar happens to accept. Findings are collected rather than asserted
    /// one by one, so a single run reports every grammar out of step with its
    /// fixture instead of stopping at the first.
    #[test]
    fn every_compiled_grammar_parses_its_fixture_and_refuses_a_malformed_one() {
        let mut failures: Vec<String> = Vec::new();
        for &language in Language::ALL {
            let name = language.name();
            let Some(fixture) = FIXTURES.iter().find(|row| row.language == name) else {
                failures.push(format!("`{name}` is compiled but has no fixture row"));
                continue;
            };
            if let Err(refusal) = try_parse(fixture.valid, language) {
                failures.push(format!("`{name}` refused its valid fixture: {refusal}"));
            }
            match try_parse(fixture.malformed, language) {
                Err(ParseError::InvalidSyntax { .. }) => {}
                Ok(_) => failures.push(format!(
                    "`{name}` accepted a malformed fixture: {:?}",
                    fixture.malformed
                )),
                Err(other) => failures.push(format!(
                    "`{name}` refused a malformed fixture as {other}, not InvalidSyntax"
                )),
            }
        }
        assert!(
            failures.is_empty(),
            "grammar fixtures disagree with their grammars: {}",
            failures.join("; ")
        );
    }

    /// The fixture table holds one row per declared grammar, with no repeats.
    ///
    /// A row nothing matches is a row that never runs, which is how a renamed
    /// grammar would drop out of the matrix silently; the count is what makes a
    /// deleted row fail here rather than shrink the matrix unnoticed.
    #[test]
    fn fixture_table_is_one_row_per_declared_grammar() {
        let mut covered: Vec<&str> = Vec::new();
        for row in FIXTURES {
            assert!(
                !covered.contains(&row.language),
                "two fixture rows cover `{}`",
                row.language
            );
            covered.push(row.language);
        }
        assert_eq!(
            covered.len(),
            DECLARED_GRAMMARS,
            "the fixture table must cover every grammar the manifest declares"
        );
    }

    /// Under `full` every declared grammar is compiled, so the table and the
    /// compiled set correspond exactly: a misspelled row name has nothing to
    /// match and is caught here rather than skipped.
    #[cfg(feature = "full")]
    #[test]
    fn fixture_table_names_exactly_the_grammars_full_compiles() {
        for row in FIXTURES {
            assert!(
                Language::ALL
                    .iter()
                    .any(|language| language.name() == row.language),
                "fixture row `{}` names no grammar that `full` compiled",
                row.language
            );
        }
        assert_eq!(
            Language::ALL.len(),
            FIXTURES.len(),
            "`full` compiles every declared grammar, so every one must have a fixture row"
        );
    }
}
