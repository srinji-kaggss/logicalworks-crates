//! The multi-language AST parser: language identification, grammar
//! selection, bounded parsing and traversal (the `parser` feature).
//!
//! Every `lang-*` feature enables `parser`, and so does the default set. A
//! build without it carries no grammar stack at all: only the typed-error
//! derive and the diagnostic types a tool reports with (#367). Everything here
//! is re-exported at the crate root, so the paths a consumer names are the
//! same either way.

use std::cell::RefCell;
use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::ops::ControlFlow;
use std::time::{Duration, Instant};

use ast_grep_core::Language as CoreLanguage;
use ast_grep_core::matcher::{Pattern, PatternBuilder, PatternError};
use ast_grep_core::tree_sitter::LanguageExt;
pub use ast_grep_core::tree_sitter::{StrDoc, TSLanguage};
pub use ast_grep_core::{AstGrep, Node};
pub use ast_grep_language::SupportLang;

use crate::diagnostic::{self, Diagnostic, Severity};

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
/// built it, so it does not cap the parser's own allocation. The walk is linear
/// in tree size, so this ceiling prices its *worst case*, not its running cost.
pub const MAX_AST_NODES: usize = 2_000_000;

/// Deepest branch a checked parse admits, root counting as 1.
///
/// [`MAX_AST_NODES`] prices a *wide* tree; a *deep* one is bounded by neither it
/// nor the byte ceiling in any useful way, because a source of `(((…` is a few
/// bytes per nesting level and yields one node per level plus the delimiters. The
/// node ceiling therefore admits a tree hundreds of thousands of levels deep,
/// and the traversal retains one frame per active ancestor, so the depth is
/// bounded explicitly instead.
///
/// The bound is on the checked parse ([`try_parse`], [`try_parse_with`]). The
/// public [`inspect_ast`] walk is uncapped in depth, because a caller inspecting
/// a tree on purpose may want the real depth of a malformed one; the ceiling's
/// only producer is the checked parse, which reports it as
/// [`ParseError::AstTooDeep`].
pub const MAX_AST_DEPTH: usize = 512;

/// How long a checked parse may hold its thread when the caller names no
/// deadline (10 s).
///
/// The byte ceiling bounds what the parser is handed, not how long it works on
/// it: tree-sitter's GLR parser is super-linear in nesting on some grammars, and
/// the parse-budget rig measured 97.5 s for 256 KiB of nested braces under the
/// Dart grammar and 29.8 s for a 512 KiB single line of Scala, with the 2 MiB
/// forms of both unfinished at 120 s (`bench/README.md`). This is the bound on
/// that: the parser is asked every hundred operations whether to continue, and
/// past the deadline it stops, releases its thread and the call answers
/// [`ParseError::TimedOut`].
///
/// Ten seconds is chosen from the same measurement. The slowest grammar on its
/// own valid source at the full byte ceiling is `javascript` at 1 MB/s p99,
/// about two seconds for 2 MiB, and the slowest *legitimate* shape measured —
/// one megabyte-and-a-half line of Ruby — is 5.6 s; ten seconds admits both on
/// the measuring host and stops every adversarial row the rig recorded. A
/// caller with a tighter budget, or a slower host, names its own through
/// [`try_parse_within`].
pub const DEFAULT_PARSE_DEADLINE: Duration = Duration::from_secs(10);

/// Grammars one thread keeps a ready parser for before it starts over.
///
/// A parser is per thread and per grammar, so the cache is never shared and
/// never locked; the ceiling exists because a caller-registered grammar is a
/// key this crate does not choose, and an unbounded map keyed by it would grow
/// with whatever a long-lived thread was handed. Every compiled grammar fits
/// with room to spare, so the built-in set never evicts.
const MAX_CACHED_PARSERS: usize = 64;

thread_local! {
    /// This thread's parser per grammar, reset before every parse.
    static PARSERS: RefCell<HashMap<TSLanguage, tree_sitter::Parser>> =
        RefCell::new(HashMap::new());
}

/// Largest number of block containers one line of markdown may declare (64).
///
/// **This is a process-safety bound, not a quality bound.** tree-sitter's
/// markdown grammar keeps its open block containers in an external scanner whose
/// state is serialized into a fixed 1 024-byte buffer, and it *asserts* — which
/// is `abort()`, not a catchable error — when that state does not fit. A source
/// that opens 255 containers does not return a `ParseError`: it ends the
/// process, with the caller inside it. `- ` repeated 255 times is 510 bytes.
///
/// The bound therefore sits well below the smallest measured abort. Bisected
/// from a child process across seventeen container shapes — unordered and
/// ordered list markers, blockquotes of one, two and three `>`, tabs,
/// list-in-quote, quote-in-list, list-in-quote-in-list, fenced and indented
/// code inside containers, and containers nested by indentation — **every
/// aborting shape reaches its abort at the same 255 open containers**, whether
/// it spells them one per repetition (255 repetitions), two per repetition
/// (128) or three (85). [`markdown_containers`]
/// counts that quantity directly, so the margin is `255 / 64`, about 4x, and
/// the count is additionally an over-estimate wherever indentation and markers
/// both contribute depth.
///
/// The cost of the margin is a refusal, not a crash: a document nested more
/// than 64 containers deep on one line is refused as
/// [`ParseError::ContainerNestingTooDeep`] before the scanner sees it. A
/// document nested 30 deep, which is already past what any real file contains,
/// still parses.
///
/// Only a grammar registered under the name `markdown` is measured against it,
/// and only on the checked parse. The count needs no grammar and no cargo
/// feature, so the rule is present in every build rather than appearing only
/// where `lang-md` happens to be on.
pub const MAX_MARKDOWN_CONTAINERS_PER_LINE: usize = 64;

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
    /// The tree is nested deeper than the checked parse admits.
    ///
    /// A separate arm from [`ParseError::AstTooLarge`], because a caller acts on
    /// them differently: a node refusal says *this source is large*, and a depth
    /// refusal says *this source is shaped like the deeply-nested input a hostile
    /// parser input is made of*. Both name the bound and the observed value, and
    /// both are refusals of the whole file rather than located diagnostics.
    #[error("{language} AST nests deeper than {limit} levels (observed at least {observed})")]
    AstTooDeep {
        /// The language name.
        language: &'static str,
        /// Branch depth observed up to the bound.
        observed: usize,
        /// The applied bound.
        limit: usize,
    },
    /// One line declares more block containers than the checked parse admits.
    ///
    /// A *pre-parse* refusal, and a separate arm from
    /// [`ParseError::AstTooDeep`] because it measures something else about
    /// something else. `AstTooDeep` counts the depth of a tree that was built;
    /// this counts containers declared by the *text*, before any grammar runs,
    /// because the grammar cannot be run at all: the markdown scanner's own
    /// serialization buffer overflows first and takes the process with it. See
    /// [`MAX_MARKDOWN_CONTAINERS_PER_LINE`].
    ///
    /// Reported before the parse rather than after it, so a caller sees a
    /// refusal it can act on instead of a process that is no longer there.
    #[error(
        "{language} declares at least {observed} block containers on one line; \
         the limit is {limit}"
    )]
    ContainerNestingTooDeep {
        /// The language name.
        language: &'static str,
        /// Containers counted on the offending line.
        observed: usize,
        /// The applied bound.
        limit: usize,
    },
    /// The parser was still working when its deadline passed, and was stopped.
    ///
    /// A refusal of the whole file, like the size and shape refusals, and not a
    /// statement about its syntax: the source may be perfectly valid and merely
    /// expensive for this grammar. The thread that ran the parse is free again
    /// when this is returned, and its parser is reset, so the next parse on the
    /// same thread starts clean rather than resuming this one.
    #[error("{language} parse stopped at its deadline of {after:?}")]
    TimedOut {
        /// The language name.
        language: &'static str,
        /// The deadline that was applied.
        after: Duration,
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
    /// Every refusal is a [`Severity::Error`]. An [`InvalidSyntax`] refusal
    /// points at the earliest recovery node it retained, using that node's byte
    /// span in `source`, so a renderer underlines the text the grammar rejected.
    /// The other refusals are conditions of the whole file (a source too large,
    /// a grammar that produced no tree, a tree over the node bound), so their
    /// span is zero-width at the end of `source` rather than an invented line.
    /// `source` must be the text that was parsed; each span end is clamped into
    /// it.
    ///
    /// [`InvalidSyntax`]: ParseError::InvalidSyntax
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
        let span = match *self {
            Self::InvalidSyntax {
                ref diagnostics, ..
            } => diagnostics
                .iter()
                .min_by_key(|found| found.start_byte)
                .map_or_else(
                    || diagnostic::end_of(source),
                    |first| diagnostic::span_of(source, first.start_byte..first.end_byte),
                ),
            _ => diagnostic::end_of(source),
        };
        Diagnostic::new(self.to_string(), span)
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
    /// The walk descended one level beyond its configured depth limit.
    ///
    /// Only the checked parse configures a depth limit — see [`MAX_AST_DEPTH`] —
    /// and it turns this into [`ParseError::AstTooDeep`] rather than handing a
    /// caller a partial [`AstMetrics`], so the variant is recorded here rather
    /// than dropped so the metrics cannot report an incomplete walk with no
    /// reason.
    DepthLimitExceeded,
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
///
/// There is no `Default`: every value comes from a walk, so `complete` always
/// says whether that walk visited the whole tree. A default would be a value no
/// walk produced, with `complete == false` reading as a partial inspection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
    /// Fold one visited node into the running metrics.
    ///
    /// Called once per node of a walk, so this is where the three accumulators
    /// are kept monotone: `nodes` saturates instead of wrapping (unreachable at
    /// any real tree size, but the bound is stated rather than assumed),
    /// `max_depth` keeps the deepest branch seen, and `has_syntax_issues` is
    /// sticky: once an `ERROR` or `MISSING` node is seen, no later clean node
    /// may clear it, because the walk has no way to unsee it.
    fn charging(mut self, visit: &Visit) -> Self {
        self.nodes = self.nodes.saturating_add(1);
        self.max_depth = self.max_depth.max(visit.depth);
        self.has_syntax_issues = self.has_syntax_issues || visit.recovery.is_some();
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
        let refusal = Err(error);
        tracing::debug!(error = ?refusal.as_ref().err(), "detect_candidates: returning an error to the caller");
        return refusal;
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

/// Production boundary: refuse oversized bytes, stop a parse past
/// [`DEFAULT_PARSE_DEADLINE`], then reject recovery nodes and over-budget trees
/// in one traversal.
pub fn try_parse(code: &str, language: Language) -> Result<Parsed, ParseError> {
    try_parse_within(code, language, DEFAULT_PARSE_DEADLINE)
}

/// [`try_parse`] under a deadline the caller names.
///
/// The deadline bounds the parser, which is the work that is super-linear on
/// hostile input; the byte check before it and the validation walk after it are
/// linear and bounded by [`MAX_SOURCE_BYTES`] and [`MAX_AST_NODES`]. A parse
/// still running when `deadline` passes is stopped at the parser's next progress
/// check — every hundred parser operations — and answers
/// [`ParseError::TimedOut`]; one that finishes first answers exactly as
/// [`try_parse`] does. A source small enough to parse before the first check
/// therefore completes even under `Duration::ZERO`, because a parse that
/// finished has spent nothing the deadline could have saved.
///
/// ```
/// use std::time::Duration;
/// use lgwks_ast::{Language, ParseError};
///
/// let nested = "fn f() {".repeat(4_000);
/// let refusal = lgwks_ast::try_parse_within(&nested, Language::Rust, Duration::ZERO);
/// assert!(matches!(refusal, Err(ParseError::TimedOut { .. })));
/// // The thread is free, and its parser starts clean.
/// assert!(lgwks_ast::try_parse_within("fn f() {}", Language::Rust, Duration::from_secs(1)).is_ok());
/// ```
pub fn try_parse_within(
    code: &str,
    language: Language,
    deadline: Duration,
) -> Result<Parsed, ParseError> {
    parse_bounded(
        code,
        &language.support_lang(),
        language.name(),
        Bounds {
            source_bytes: MAX_SOURCE_BYTES,
            ast_nodes: MAX_AST_NODES,
            ast_depth: MAX_AST_DEPTH,
            deadline,
        },
    )
}

/// Checked parse of a caller-registered grammar, held to the same byte bound,
/// deadline, node bound, depth bound, and recovery refusal as [`try_parse`].
pub fn try_parse_with<L: LanguageExt>(
    code: &str,
    language: &L,
    name: &'static str,
) -> Result<AstGrep<StrDoc<L>>, ParseError> {
    parse_bounded(
        code,
        language,
        name,
        Bounds {
            source_bytes: MAX_SOURCE_BYTES,
            ast_nodes: MAX_AST_NODES,
            ast_depth: MAX_AST_DEPTH,
            deadline: DEFAULT_PARSE_DEADLINE,
        },
    )
}

/// The policy one checked parse applies, named at the call site.
#[derive(Clone, Copy)]
struct Bounds {
    /// Largest source handed to the parser.
    source_bytes: usize,
    /// Largest tree the walk admits.
    ast_nodes: usize,
    /// Deepest branch the walk admits.
    ast_depth: usize,
    /// How long the parser may run.
    deadline: Duration,
}

/// The one body behind [`try_parse`] and [`try_parse_with`]: byte bound, then
/// the tree under its deadline, then node bound, then depth bound, then
/// recovery refusal.
///
/// The bounds are parameters rather than constants read in place so both entry
/// points state the policy they apply at the call site, and so a later caller
/// with a different budget reuses this ordering instead of restating it. The
/// order is the point: the byte check runs before the parser allocates, the
/// node and depth checks before any walk of the tree reaches a caller, and both
/// before the recovery check, so a refusal never describes a tree this walk did
/// not finish inspecting.
///
/// The two tree bounds are separate refusals because they price different
/// shapes: [`ParseError::AstTooLarge`] is a wide tree, and
/// [`ParseError::AstTooDeep`] is a narrow one whose source is a few bytes per
/// nesting level, which the byte ceiling admits in the hundreds of thousands of
/// levels and the node ceiling admits in full.
///
/// `language` is borrowed and cloned into the `StrDoc` because a grammar may be
/// a registered [`CustomLang`] or a built-in [`Language`]; `name` is the stable
/// name the error variants carry, since they cannot hold the grammar itself.
fn parse_bounded<L: LanguageExt>(
    code: &str,
    language: &L,
    name: &'static str,
    bounds: Bounds,
) -> Result<AstGrep<StrDoc<L>>, ParseError> {
    let Bounds {
        source_bytes: max_source_bytes,
        ast_nodes: max_ast_nodes,
        ast_depth: max_ast_depth,
        deadline,
    } = bounds;
    validate_source_size(code, max_source_bytes)?;
    // Before the parser, and only for the markdown grammar: its external scanner
    // serializes open block containers into a fixed buffer and asserts when they
    // do not fit, which ends the process rather than returning an error. Every
    // other grammar reaches `try_new` on any source inside the byte bound.
    if name == MARKDOWN {
        validate_markdown_containers(code, name, MAX_MARKDOWN_CONTAINERS_PER_LINE)?;
    }
    let tree = parse_tree(code, &language.get_ts_language(), name, deadline);
    let parsed = AstGrep::doc(StrDoc {
        src: code.to_owned(),
        lang: language.clone(),
        tree: tree?,
    });
    let (metrics, _, diagnostics, diagnostics_truncated) =
        inspect_ast_with_pending(&parsed.root(), Some(max_ast_nodes), Some(max_ast_depth));
    if metrics.stop_reason == Some(InspectionStopReason::DepthLimitExceeded) {
        let refusal = Err(ParseError::AstTooDeep {
            language: name,
            observed: metrics.max_depth,
            limit: max_ast_depth,
        });
        tracing::debug!(error = ?refusal.as_ref().err(), "parse_bounded: returning an error to the caller");
        return refusal;
    }
    if !metrics.complete {
        let refusal = Err(ParseError::AstTooLarge {
            language: name,
            observed: metrics.nodes,
            limit: max_ast_nodes,
        });
        tracing::debug!(error = ?refusal.as_ref().err(), "parse_bounded: returning an error to the caller");
        return refusal;
    }
    if metrics.has_syntax_issues {
        let refusal = Err(ParseError::InvalidSyntax {
            language: name,
            diagnostics,
            diagnostics_truncated,
        });
        tracing::debug!(error = ?refusal.as_ref().err(), "parse_bounded: returning an error to the caller");
        return refusal;
    }
    Ok(parsed)
}

/// Build `code`'s tree with this thread's parser for `grammar`, stopping it at
/// `deadline`.
///
/// This is the one place the crate names `tree-sitter` directly, and it does so
/// for the one thing `ast-grep-core` does not expose: its `parse_lang` builds the
/// parser privately and calls `Parser::parse` with no progress callback, so a
/// tree built through it cannot be stopped. The tree built here is the same
/// `tree_sitter::Tree` ast-grep would have built — same grammar, same source,
/// same parser defaults — and is handed to it through `StrDoc`'s public fields,
/// so a caller receives the identical [`Parsed`].
///
/// The parser is cached per thread and per grammar, as ast-grep's is. If the
/// cache is already borrowed on this thread, which nothing in this crate does,
/// the parse runs on a fresh parser rather than failing.
fn parse_tree(
    code: &str,
    grammar: &TSLanguage,
    name: &'static str,
    deadline: Duration,
) -> Result<tree_sitter::Tree, ParseError> {
    PARSERS.with(|cache| match cache.try_borrow_mut() {
        Ok(mut parsers) => {
            if parsers.len() >= MAX_CACHED_PARSERS && !parsers.contains_key(grammar) {
                parsers.clear();
            }
            let parser = match parsers.entry(grammar.clone()) {
                Entry::Occupied(entry) => entry.into_mut(),
                Entry::Vacant(entry) => {
                    let fresh = new_parser(grammar, name);
                    entry.insert(fresh?)
                }
            };
            run_parser(parser, code, name, deadline, &mut Instant::now)
        }
        Err(busy) => {
            tracing::debug!(
                ?busy,
                "parse_tree: the parser cache is in use; parsing on a fresh parser"
            );
            let fresh = new_parser(grammar, name);
            run_parser(&mut fresh?, code, name, deadline, &mut Instant::now)
        }
    })
}

/// A parser for `grammar`, or the grammar's own refusal of it.
fn new_parser(grammar: &TSLanguage, name: &'static str) -> Result<tree_sitter::Parser, ParseError> {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(grammar)
        .map(|()| parser)
        .map_err(|error| {
            let refusal = ParseError::ParserUnavailable {
                language: name,
                detail: error.to_string(),
            };
            tracing::debug!(error = ?refusal, "new_parser: returning an error to the caller");
            refusal
        })
}

/// Run one parse of `code` on `parser`, asking at every progress check whether
/// `deadline` has passed.
///
/// `clock` is read once when the parse starts and once at every progress check;
/// the callers pass `Instant::now`. It is an argument so the stopping rule can be
/// proved on a clock the proof controls: on the host's clock, how long a stopped
/// parse took measures the scheduler as much as the parser, and a 100 ms
/// deadline answered after 392 ms on a loaded CI runner although the parser
/// stopped at its first check past the deadline.
///
/// The parser is reset before and after. Before, because a parser from the
/// cache may hold state from whatever ran on it last; after, because a stopped
/// parse keeps its partial state *for resuming*, and a cached parser that kept
/// it would hand the next caller on this thread a continuation of somebody
/// else's source.
fn run_parser(
    parser: &mut tree_sitter::Parser,
    code: &str,
    name: &'static str,
    deadline: Duration,
    clock: &mut impl FnMut() -> Instant,
) -> Result<tree_sitter::Tree, ParseError> {
    parser.reset();
    // `None` is a deadline past what the clock can represent, which never
    // arrives: the parse runs to completion, as it would with no bound at all.
    let stop_at = clock().checked_add(deadline);
    let mut expired = false;
    let mut progress = |_: &tree_sitter::ParseState| {
        if stop_at.is_some_and(|at| clock() >= at) {
            expired = true;
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    };
    let bytes = code.as_bytes();
    let tree = parser.parse_with_options(
        // The parser reads the source through a closure that must answer with a
        // slice, so the end of the source is expressed as an empty one. Clamping
        // the offset to the source's length is what makes the split total: an
        // offset at or past the last byte then yields exactly the empty slice,
        // which is what the parser reads as the end of its input.
        &mut |offset, _| bytes.split_at(offset.min(bytes.len())).1,
        None,
        Some(tree_sitter::ParseOptions::new().progress_callback(&mut progress)),
    );
    parser.reset();
    tree.ok_or_else(|| {
        let refusal = if expired {
            ParseError::TimedOut {
                language: name,
                after: deadline,
            }
        } else {
            ParseError::ParserUnavailable {
                language: name,
                detail: "the parser returned no tree".to_owned(),
            }
        };
        tracing::debug!(error = ?refusal, "run_parser: returning an error to the caller");
        refusal
    })
}

/// Unchecked parse for diagnostics and tests that intentionally inspect
/// malformed trees. Production call sites use [`try_parse`].
///
/// **This path does not run [`MAX_MARKDOWN_CONTAINERS_PER_LINE`].** It returns a
/// [`Parsed`] rather than a `Result`, so a refusal has nowhere to go here, and a
/// markdown source past that bound will abort the process inside the grammar's
/// external scanner rather than return anything. It also runs neither
/// [`MAX_AST_NODES`] nor [`MAX_AST_DEPTH`]. A caller accepting untrusted input
/// uses [`try_parse`], which is the path every bound lives on.
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

/// The name the markdown guard is keyed on.
///
/// A caller registering its own grammar chooses this name, so a custom grammar
/// called `markdown` is measured by the same rule. That is the intended reading:
/// the name is the stable identity a caller matches findings on, and a caller who
/// claims to be markdown gets markdown's bound.
const MARKDOWN: &str = "markdown";

/// The containers declared on the first line of `source` that exceeds `limit`,
/// or `None` when no line does.
///
/// The count is an **upper bound** on the scanner's open containers, for two
/// reasons, and both push the same way:
///
/// - Indentation and markers can both carry nesting, so their depths are added
///   rather than the larger taken. A list nested by indentation opens one
///   container per line and adds depth through its indent; a line carrying both
///   is charged for both.
/// - A marker is counted wherever it appears in a line, not only where it could
///   legally open a container. `a - b - c` counts two. That is prose, not
///   nesting, and refusing it costs nothing because the bound is 64; the point
///   is that a count which is *too high* is a refusal and a count which is too
///   low is a crash.
///
/// One pass over the bytes, and four `usize`s of state: no line is buffered, no
/// allocation is made, and the walk stops at the first line over the bound, so a
/// hostile source costs `O(bytes)` and nothing else. Returning `None` for a
/// source inside the bound is the whole contract — the deepest count *within* the
/// bound is not interesting to a caller, who has already been told it passed.
#[must_use]
pub fn markdown_containers(source: &str, limit: usize) -> Option<usize> {
    for line in source.split('\n') {
        let (indent_columns, rest) = split_indent(line);
        // Two spaces per level of indentation-nested container: the narrowest
        // indent a real nested list carries. A tab is four columns, which is the
        // width every markdown tool agrees on for one. The divisor is a
        // non-zero constant, so the division saturates rather than standing in a
        // column count for a divisor that does not exist.
        let from_indent = indent_columns.saturating_div(2);
        let counted = from_indent.saturating_add(count_markers(rest));
        if counted > limit {
            return Some(counted);
        }
    }
    None
}

/// The leading whitespace of `line` in columns, and the line without it.
fn split_indent(line: &str) -> (usize, &str) {
    let mut columns = 0_usize;
    for (at, character) in line.char_indices() {
        match character {
            ' ' => columns = columns.saturating_add(1),
            '\t' => columns = columns.saturating_add(4),
            _ => return (columns, &line[at..]),
        }
    }
    (columns, "")
}

/// The block containers `line` declares, after its indentation.
///
/// Counts, per markdown's block grammar: every `>` (a run of them is that many
/// levels of blockquote), every `-`, `*` or `+` immediately followed by a space
/// or the end of the line (a list marker needs that space; `---` is a thematic
/// break and `*emphasis*` is not a list), every digit run followed by `.` or `)`
/// and then that same space, and one for a line that opens a code fence.
///
/// Markers are counted **anywhere** in the line, not only where one could
/// legally open a container. `1. 1. 1. x` is three nested ordered lists and
/// `a - b - c` is prose that happens to contain two dashes; the second is
/// counted and the bound is 64, so the price of the over-count is a refusal on
/// a line nobody writes, while the price of an under-count is a process that is
/// no longer there.
fn count_markers(line: &str) -> usize {
    if opens_code_fence(line) {
        return 1;
    }
    let mut counted = 0_usize;
    let mut rest = line;
    while let Some(character) = rest.chars().next() {
        let tail = &rest[character.len_utf8()..];
        if character == '>' {
            counted = counted.saturating_add(1);
        } else if matches!(character, '-' | '*' | '+') {
            if is_marker_end(tail) {
                counted = counted.saturating_add(1);
            }
        } else if character.is_ascii_digit() {
            counted = counted.saturating_add(ordered_marker_len(rest));
        }
        rest = tail;
    }
    counted
}

/// Whether `tail` begins the space or end of line a list marker needs.
fn is_marker_end(tail: &str) -> bool {
    tail.is_empty() || tail.starts_with([' ', '\t'])
}

/// How many containers one ordered-list marker at the head of `rest` declares.
///
/// `1.` and `1)` open a list; `1.5` is a number, `v1.2` is prose, and `1234567.`
/// is a list whose item number is seven digits long. The marker is one
/// container however long its number is, which is why this counts a container
/// and not the digits it consumed.
fn ordered_marker_len(rest: &str) -> usize {
    let digits = rest
        .char_indices()
        .find(|entry| !entry.1.is_ascii_digit())
        .map_or(rest.len(), |(offset, _)| offset);
    let after_digits = &rest[digits..];
    if after_digits
        .strip_prefix(['.', ')'])
        .is_some_and(is_marker_end)
    {
        1
    } else {
        0
    }
}

/// Whether `line` opens a fenced code block.
fn opens_code_fence(line: &str) -> bool {
    let fence = line.trim_start();
    let run = fence
        .chars()
        .take_while(|marker| *marker == '`' || *marker == '~')
        .count();
    run >= 3
}

/// Refuse a markdown source whose container nesting the scanner could not
/// serialize, before the scanner is handed it.
///
/// The refusal is [`ParseError::ContainerNestingTooDeep`]; `None` means the
/// source is inside [`MAX_MARKDOWN_CONTAINERS_PER_LINE`] and the grammar may see
/// it. The check is `O(bytes)` with `O(1)` state, and it runs after the byte
/// bound so an oversized source is still refused as oversized.
fn validate_markdown_containers(
    source: &str,
    name: &'static str,
    limit: usize,
) -> Result<(), ParseError> {
    let Some(observed) = markdown_containers(source, limit) else {
        return Ok(());
    };
    let refusal = Err(ParseError::ContainerNestingTooDeep {
        language: name,
        observed,
        limit,
    });
    tracing::warn!(
        language = name,
        observed,
        limit,
        "validate_markdown_containers: refusing before the grammar's external scanner, whose \
         serialization buffer would overflow and abort the process"
    );
    refusal
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
///
/// This walk is uncapped in depth, so `max_depth` is the real depth of the
/// tree and a malformed one is measured rather than refused. [`MAX_AST_DEPTH`]
/// is the checked parse's ceiling and has no parameter here on purpose: a
/// caller inspecting a tree on purpose is not the hostile-input case, and
/// refusing it would make the reported depth a constant.
#[must_use]
pub fn inspect_ast<'t, L: LanguageExt>(
    root: &AstNode<'t, L>,
    stop_after_nodes: Option<usize>,
) -> AstMetrics {
    inspect_ast_with_pending(root, stop_after_nodes, None).0
}

/// Inspect in one cursor-driven pre-order pass, retaining no sibling frontier.
///
/// The walk drives a tree-sitter cursor directly. The cursor owns a stack of the
/// ancestors it has descended through and nothing else, so the traversal
/// retains state proportional to active depth and never to a node's fan-out
/// (#277). The previous hand-rolled frame walk reached each child through
/// `ast_grep_core::Node::child` by index, and that call is `O(index)` on a node
/// whose visible children are not its structural ones — which is exactly what a
/// recovery-heavy parse produces. The measured cost was quadratic in fan-out: a
/// 16 KiB source of unbalanced delimiters, 16 385 nodes wide and 2 deep, spent
/// 1.39 s in the walk against 1.06 ms in the parser, and doubling the width
/// quadrupled the walk. The cursor walk spends 40 ns per node on that same
/// source, at every width from 2 KiB to 2 MiB.
///
/// The cursor is reached through the node the walk was handed, so the walk
/// names no `tree-sitter` type: every call below is an inherent method on a type
/// this crate already holds. (The crate's one direct `tree-sitter` use is the
/// parse deadline in `parse_tree`, which `ast-grep-core` has no door for.)
///
/// Two properties move with the traversal and are worth stating rather than
/// leaving to be discovered:
///
/// - **Order.** Nodes arrive in source order, not reverse-sibling order. Every
///   published guarantee survives it: [`AstMetrics`] folds order-independently,
///   and the retained diagnostics are the earliest ones under
///   [`MAX_SYNTAX_DIAGNOSTICS`] and are sorted by source position before they
///   are returned. What changes is only which node is the `limit + 1` witness,
///   and it is now the one earliest in the file rather than the last.
/// - **Frames.** The second return value is `peak_depth`: the deepest cursor
///   stack the walk held, which is the number of active ancestors and therefore
///   the memory the traversal actually retains. It is kept private as a
///   directly asserted resource invariant rather than exported as a public
///   metric whose callers might mistake it for a configured limit.
fn inspect_ast_with_pending<'t, L: LanguageExt>(
    root: &AstNode<'t, L>,
    stop_after_nodes: Option<usize>,
    stop_after_depth: Option<usize>,
) -> (AstMetrics, usize, Vec<SyntaxDiagnostic>, bool) {
    let mut metrics = AstMetrics {
        nodes: 0,
        max_depth: 0,
        has_syntax_issues: false,
        complete: true,
        node_limit: stop_after_nodes,
        stop_reason: None,
    };
    let mut diagnostics = Vec::new();
    let mut diagnostics_truncated = false;
    let mut cursor = root.get_inner_node().walk();
    // The cursor starts on the root, which counts as depth 1, so its zero-based
    // depth is always one less than the depth a caller reads.
    let mut cursor_depth = 0_usize;
    let mut peak_depth = 0_usize;
    loop {
        let node = cursor.node();
        let span = node.range();
        let recovery = if node.is_error() {
            Some(SyntaxIssueKind::Error)
        } else if node.is_missing() {
            Some(SyntaxIssueKind::Missing)
        } else {
            None
        };
        let visit = Visit {
            depth: cursor_depth.saturating_add(1),
            recovery,
            span,
        };
        peak_depth = peak_depth.max(visit.depth);
        metrics = metrics.charging(&visit);
        // The witness node is charged before the walk ends, so a refusal that
        // stops at a ceiling still reports the damage it saw on the way to the
        // ceiling rather than dropping it.
        push_recovery_span(&visit, &mut diagnostics, &mut diagnostics_truncated);
        if let Some(reason) = charge_reason(&metrics, stop_after_nodes, stop_after_depth) {
            mark_inspection_incomplete(&mut metrics, reason);
            break;
        }
        // Descend into the first child, or climb until a sibling exists. The
        // cursor's own stack is the ancestor record, so unwinding costs one step
        // per level rather than a search for the parent.
        if cursor.goto_first_child() {
            cursor_depth = cursor_depth.saturating_add(1);
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                diagnostics.sort_unstable_by_key(syntax_position);
                return (metrics, peak_depth, diagnostics, diagnostics_truncated);
            }
            cursor_depth = cursor_depth.saturating_sub(1);
        }
    }
    diagnostics.sort_unstable_by_key(syntax_position);
    (metrics, peak_depth, diagnostics, diagnostics_truncated)
}

/// One node as the walk read it, detached from the cursor that produced it.
///
/// The walk asks three questions of every node and keeps nothing else, so the
/// facts are copied out here rather than a node handle: a handle borrows the
/// cursor's position, and a list of them would be the sibling frontier this
/// traversal exists not to build.
struct Visit {
    /// One-based branch depth, the root counting as 1.
    depth: usize,
    /// The recovery kind this node carries, or `None` for a clean node.
    recovery: Option<SyntaxIssueKind>,
    /// Inclusive start and exclusive end byte offsets in the source, in the
    /// node type's own range so no conversion sits in the walk's inner loop.
    span: ast_grep_core::tree_sitter::TSRange,
}

/// Which cap this observation already exceeded, if either.
///
/// Evaluated after every node, the root included, so a ceiling at or below one
/// stops the walk there and the overflow witness is charged before the walk
/// ends — the accounting the previous root-only check gave, applied at every
/// level. With both ceilings configured the node one is reported, because the
/// node charge is the first the root trips and a caller reading one reason
/// wants the one that fired first.
fn charge_reason(
    metrics: &AstMetrics,
    stop_after_nodes: Option<usize>,
    stop_after_depth: Option<usize>,
) -> Option<InspectionStopReason> {
    if stop_after_nodes.is_some_and(|limit| metrics.nodes > limit) {
        return Some(InspectionStopReason::NodeLimitExceeded);
    }
    stop_after_depth
        .is_some_and(|limit| metrics.max_depth > limit)
        .then_some(InspectionStopReason::DepthLimitExceeded)
}

/// Mark a walk as partial after it observes the value beyond its configured cap.
fn mark_inspection_incomplete(metrics: &mut AstMetrics, reason: InspectionStopReason) {
    metrics.complete = false;
    metrics.stop_reason = Some(reason);
}

/// Retain one recovery-node span until the public diagnostic ceiling is reached.
fn push_recovery_span(
    visit: &Visit,
    diagnostics: &mut Vec<SyntaxDiagnostic>,
    diagnostics_truncated: &mut bool,
) {
    let Some(kind) = visit.recovery else {
        return;
    };
    let span = visit.span;
    push_syntax_diagnostic(
        SyntaxDiagnostic {
            kind,
            start_byte: span.start_byte,
            end_byte: span.end_byte,
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
/// The ceiling is a constant, so something has to be dropped once it is
/// reached, and the error a reader needs first — where the source stopped
/// parsing — is the one that must not be it. At the ceiling a new diagnostic
/// replaces the latest one retained when it starts earlier; the retained set is
/// always the earliest in source order, in constant memory, whatever order the
/// walk reached them in. (The cursor-driven walk reaches them in source order
/// already; the rule is what keeps the property true of any traversal order,
/// including a caller-supplied one.)
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
        // INV-AST-1, on the adversarial width: 4 096 immediate siblings under
        // the root, which is the shape a sibling frontier would have to be
        // sized for. The walk stops at the cap after two nodes, so the retained
        // state must be the two levels of cursor stack it descended through and
        // nothing resembling the 4 096 children behind the first of them.
        let source = (0..4_096)
            .map(|index| format!("fn f{index}() {{}}\n"))
            .collect::<String>();
        let parsed = parse(&source, Language::Rust);
        assert_eq!(
            parsed.root().children().len(),
            4_096,
            "the fixture presents 4,096 immediate siblings to the traversal"
        );

        let (metrics, peak_depth, _, _) = inspect_ast_with_pending(&parsed.root(), Some(1), None);

        assert_eq!(metrics.nodes, 2, "one node beyond the cap proves refusal");
        assert_eq!(
            inspect_ast(&parsed.root(), Some(1)).nodes,
            2,
            "the public inspector preserves the budget's overflow witness"
        );
        assert_eq!(
            peak_depth, 2,
            "retained a cursor stack {peak_depth} deep for a 4,096-child root, \
             which is a sibling frontier rather than a depth"
        );
        assert!(
            peak_depth < 16,
            "the retained depth must not scale with the root's {width} children",
            width = parsed.root().children().len()
        );
    }

    #[test]
    fn a_node_budget_charges_children_in_source_order() {
        // Which node is the `limit + 1` witness is the one thing the cursor walk
        // changed, and it is stated rather than left implicit: children arrive
        // in source order, so a cap below the tree size charges the *earliest*
        // children. Both halves matter — a witness taken from the end of the
        // file could report damage a caller would never see, and one taken from
        // the front cannot.
        let parsed = parse(
            "fn first() {}\nfn second() {}\nfn third() {}\n",
            Language::Rust,
        );
        let root = parsed.root();
        assert!(
            root.children().len() >= 3,
            "the fixture presents at least three root children to the traversal"
        );

        let metrics = inspect_ast(&root, Some(1));

        assert_eq!(metrics.nodes, 2, "the overflow witness is counted");
        assert!(
            !metrics.has_syntax_issues,
            "the first child of valid source is clean, so the cap charged a clean \
             witness rather than one from the end of the file"
        );
        assert_eq!(
            metrics.stop_reason,
            Some(InspectionStopReason::NodeLimitExceeded),
            "the cap, not a clean tree, is what stopped the walk"
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
        // ancestors is the depth, and the state retained must be that depth
        // rather than the total node count.
        let depth = 200;
        let source = format!(
            "{}fn f() {{}}{}",
            "fn f() {".repeat(depth),
            "}".repeat(depth)
        );
        let parsed = parse(&source, Language::Rust);
        let (metrics, peak_depth, _, _) = inspect_ast_with_pending(&parsed.root(), None, None);

        assert!(
            metrics.max_depth > 50,
            "the fixture is {} deep, so the walk really does descend",
            metrics.max_depth
        );
        assert_eq!(
            peak_depth, metrics.max_depth,
            "retained a stack {peak_depth} deep for a tree {} deep; the walk is \
             not retaining exactly the active ancestors",
            metrics.max_depth
        );
        assert!(
            metrics.nodes > peak_depth.saturating_mul(4),
            "retained a stack {peak_depth} deep for a tree of {} nodes, which is \
             a sibling frontier rather than a depth",
            metrics.nodes
        );
    }

    /// The walk this crate replaced, kept as the model the cursor walk is
    /// checked against.
    ///
    /// It is the frame walk from before #277, verbatim in its traversal: a
    /// stack of `(node, depth, next child index)` with children taken by index,
    /// descending into the highest index first. It exists so the replacement can
    /// be shown to visit the same nodes at the same depths rather than merely
    /// to be shown green, and it is `#[cfg(test)]` because nothing ships it.
    fn positional_walk<L: LanguageExt>(root: &AstNode<'_, L>) -> (usize, usize, usize, bool) {
        let mut frames = vec![(root.clone(), 1_usize, root.children().len())];
        let mut nodes = 1_usize;
        let mut peak_frames = 1_usize;
        let mut deepest = 1_usize;
        let mut issues = root.is_error() || root.is_missing();
        while let Some(frame) = frames.last_mut() {
            let Some(child_index) = frame.2.checked_sub(1) else {
                frames.pop();
                continue;
            };
            frame.2 = child_index;
            let Some(child) = frame.0.child(child_index) else {
                continue;
            };
            let child_depth = frame.1.saturating_add(1);
            nodes = nodes.saturating_add(1);
            deepest = deepest.max(child_depth);
            issues = issues || child.is_error() || child.is_missing();
            let child_count = child.children().len();
            frames.push((child, child_depth, child_count));
            peak_frames = peak_frames.max(frames.len());
        }
        (nodes, deepest, peak_frames, issues)
    }

    #[test]
    fn the_cursor_walk_visits_exactly_the_nodes_the_positional_walk_did() {
        // The differential evidence for #277. The traversal changed; what it
        // must not have changed is the answer. Five trees chosen for the shapes
        // that separate the two: flat and wide, deep and narrow, recovery-heavy,
        // empty, and a real function.
        let wide = (0..4_096)
            .map(|index| format!("fn f{index}() {{}}\n"))
            .collect::<String>();
        let deep = format!("{}fn f() {{}}{}", "fn f() {".repeat(200), "}".repeat(200));
        let fixtures = [
            String::new(),
            String::from("fn main() {}\n"),
            wide,
            deep,
            String::from("@\nfn broken( {\n"),
        ];
        for (index, source) in fixtures.iter().enumerate() {
            let parsed = parse(source.as_str(), Language::Rust);
            let root = parsed.root();
            let (model_nodes, model_depth, model_peak, model_issues) = positional_walk(&root);
            let (metrics, peak_depth, _, _) = inspect_ast_with_pending(&root, None, None);

            assert_eq!(
                metrics.nodes, model_nodes,
                "fixture {index}: {} nodes visited against the model's {model_nodes}",
                metrics.nodes
            );
            assert_eq!(
                metrics.max_depth, model_depth,
                "fixture {index}: depth {} against the model's {model_depth}",
                metrics.max_depth
            );
            assert_eq!(
                peak_depth, model_peak,
                "fixture {index}: retained {peak_depth} against the model's \
                 {model_peak} active ancestors"
            );
            assert_eq!(
                metrics.has_syntax_issues, model_issues,
                "fixture {index}: recovery state disagrees with the model"
            );
        }
    }

    #[test]
    fn the_walk_costs_the_same_per_node_however_wide_the_tree_is()
    -> Result<(), Box<dyn std::error::Error>> {
        // The measured regression behind #277, asserted as a bound rather than
        // a timing: the old walk indexed children, which is `O(index)` on a
        // recovery-heavy tree, so the per-node cost grew with the root's width.
        // Four widths of unbalanced delimiters — the shape that produced
        // 85 microseconds per node — must now cost within a small constant of
        // each other. The factor is generous on purpose: this asserts a
        // complexity class, and a wall-clock ratio is evidence of one, not a
        // benchmark. A regression to index-addressed children would multiply
        // the wide case by the width ratio (16x here) and fail by an order of
        // magnitude rather than by a hair.
        //
        // Each width is walked in `ROUNDS` windows interleaved with the others
        // and judged at its minimum, because descheduling noise is one-sided:
        // a descheduled window can only inflate a width's cost, never deflate
        // it, so the minimum is the width's cost with the host's interference
        // removed. The median was tried first and failed on a loaded CI
        // runner (run 37635420955): five of nine 1,024-node windows caught a
        // deschedule, each a tens-of-microseconds window where one preemption
        // multiplies the reading a hundredfold, and the median moved 6.5x
        // while no width scaled. A regression to index-addressed children
        // would multiply the wide case by the width ratio (16x here) at every
        // window including the minimum, and still fail the 4x bound by an
        // order of magnitude rather than by a hair.
        const WIDTHS: [usize; 4] = [1_024, 2_048, 4_096, 16_384];
        const ROUNDS: usize = 9;
        let sources = WIDTHS.map(|width| "(".repeat(width));
        let parsed = sources
            .iter()
            .map(|source| parse(source, Language::Rust))
            .collect::<Vec<_>>();
        let roots = parsed.iter().map(Parsed::root).collect::<Vec<_>>();
        let mut divisors = Vec::with_capacity(roots.len());
        for root in &roots {
            // The root counts as a node, so the model never reports zero and the
            // divisor below is a count rather than a stand-in for one.
            let nodes = positional_walk(root).0.max(1);
            divisors.push((nodes, nanos_per(nodes)?));
        }
        let mut windows = [[0_u128; WIDTHS.len()]; ROUNDS];
        for round in &mut windows {
            for ((window, root), &(nodes, divisor)) in round.iter_mut().zip(&roots).zip(&divisors) {
                let started = std::time::Instant::now();
                let metrics = inspect_ast(root, None);
                let elapsed = started.elapsed().as_nanos();
                assert_eq!(metrics.nodes, nodes, "the model and the walk disagree");
                *window = elapsed.saturating_div(divisor);
            }
        }
        let mut per_node_nanos = Vec::with_capacity(WIDTHS.len());
        for width in 0..WIDTHS.len() {
            let cheapest_window = windows
                .iter()
                .map(|round| round[width])
                .min()
                .ok_or("nine rounds produced no window for a width")?;
            per_node_nanos.push(cheapest_window);
        }
        let cheapest = per_node_nanos
            .iter()
            .copied()
            .min()
            .ok_or("four widths produced no per-node cost")?;
        let dearest = per_node_nanos
            .iter()
            .copied()
            .max()
            .ok_or("four widths produced no per-node cost")?;
        assert!(
            dearest <= cheapest.saturating_mul(4),
            "per-node walk cost {cheapest}..={dearest} ns across widths \
             {WIDTHS:?}; the cost must not scale with fan-out"
        );
        Ok(())
    }

    /// `nodes` as the divisor a per-node cost is taken over.
    ///
    /// The node count is a `usize` because the walk counts in one, and a per-node
    /// nanosecond figure is a `u128` because a wide tree's elapsed time is; the
    /// conversion is stated here rather than substituted for at each call site.
    fn nanos_per(nodes: usize) -> Result<u128, Box<dyn std::error::Error>> {
        let divisor = u128::try_from(nodes)?;
        if divisor == 0 {
            let refusal = Err("a per-node cost over no nodes is not a cost".into());
            tracing::debug!(error = ?refusal.as_ref().err(), "nanos_per: returning an error to the caller");
            return refusal;
        }
        Ok(divisor)
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
        // The recovery node has to be *beyond* the cap now that children are
        // charged in source order: `fn a() {}` on the first line puts two clean
        // nodes in front of it, so a cap of one stops before the `@`.
        let parsed = parse("fn a() {}\nfn b() {}\n@\n", Language::Rust);
        let root = parsed.root();
        assert!(
            root.children().last().is_some_and(|node| node.is_error()),
            "the fixture puts a recovery node in the last root-child position"
        );

        let partial = inspect_ast(&root, Some(1));
        assert!(
            !partial.complete,
            "the small cap stops before the late recovery node"
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

    /// Issue #211: a `MISSING` diagnostic's span is zero-width, and the README
    /// must not call it a caret span that underlines the fault.
    ///
    /// This is the negative control for the documentation repair. The shipped
    /// README promised diagnostics carry "a caret span, so a caller reports
    /// where the source stopped making sense", but a `MISSING` node marks an
    /// insertion point rather than text: it has no bytes to underline. The
    /// measured width is asserted here rather than asserted in prose, so a
    /// future change to the span calculation has to update this test and the
    /// sentence together instead of silently making the docs true or false.
    #[test]
    fn a_missing_recovery_node_carries_a_zero_width_span() -> Result<(), Box<dyn std::error::Error>>
    {
        // A missing `;` after a `let` binding: the grammar recovered by
        // inserting the token, so there is no source text at the node.
        let source = "fn main() {\n    let x = 1\n}\n";
        let diagnostics = match try_parse(source, Language::Rust) {
            Err(ParseError::InvalidSyntax { diagnostics, .. }) => diagnostics,
            other => {
                return Err(format!("expected InvalidSyntax, got {:?}", other.err()).into());
            }
        };
        let missing = diagnostics
            .iter()
            .find(|diagnostic| diagnostic.kind == SyntaxIssueKind::Missing)
            .ok_or("an omitted semicolon must produce a MISSING node")?;
        assert_eq!(
            missing.start_byte, missing.end_byte,
            "a MISSING node marks an insertion point, so its span must be empty"
        );
        assert_eq!(
            source
                .get(missing.start_byte..missing.end_byte)
                .map(str::len),
            Some(0),
            "the span must cover no source text, so it cannot underline anything"
        );

        // The offset is still addressable, so the line and column a caller
        // renders from it remain meaningful even though the span is empty.
        assert!(
            missing.start_byte <= source.len() && source.is_char_boundary(missing.start_byte),
            "the insertion point is a character boundary inside the source"
        );
        Ok(())
    }

    #[test]
    fn an_invalid_syntax_diagnostic_points_at_the_earliest_recovery_node()
    -> Result<(), Box<dyn std::error::Error>> {
        let source = "fn main() {}\nfn broken( {\nfn also( {\n";
        let refusal = match try_parse(source, Language::Rust) {
            Err(refusal @ ParseError::InvalidSyntax { .. }) => refusal,
            other => {
                return Err(format!("expected InvalidSyntax, got {:?}", other.err()).into());
            }
        };
        let reported = refusal.to_diagnostic("broken.rs", source);
        assert_eq!(reported.severity(), Severity::Error);
        assert_eq!(
            reported.span().start.line,
            2,
            "the rendered refusal names the first broken line, not end of file"
        );
        assert!(
            reported.span().byte_range().start < source.len(),
            "the span is inside the source"
        );
        assert!(
            reported.render().starts_with("broken.rs:2:"),
            "the rendered form carries the location: {}",
            reported.render()
        );

        let whole_file = ParseError::AstTooLarge {
            language: "Rust",
            observed: 9,
            limit: 8,
        }
        .to_diagnostic("big.rs", source);
        assert!(
            whole_file.span().is_empty() && whole_file.span().start.byte == source.len(),
            "a whole-file refusal stays zero-width at the end"
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
        let (metrics, peak_depth, _, _) =
            inspect_ast_with_pending(&root, Some(MAX_AST_NODES), Some(8));
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

    /// One row of the fixture table: the grammar's stable name, source it must
    /// accept, and source it must refuse.
    ///
    /// A tuple rather than a struct with three named fields because the table is
    /// twenty-eight rows of data and a row's shape says nothing the type does
    /// not: every reader destructures it, so each use names the field it reads.
    type GrammarFixture = (&'static str, &'static str, &'static str);

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
    ///
    /// ```text
    /// (name, valid source, malformed source)
    /// ```
    const FIXTURES: &[GrammarFixture] = &[
        ("bash", "echo hello\n", "echo \"unterminated\n"),
        (
            "c",
            "int main(void) { return 0; }\n",
            "int main(void) { return 0;\n",
        ),
        (
            "cpp",
            "int main() { return 0; }\n",
            "int main() { return 0;\n",
        ),
        ("csharp", "class A { }\n", "class A {\n"),
        ("css", "a { color: red; }\n", "a { color: red;\n"),
        ("dart", "void main() {}\n", "void main() {\n"),
        (
            "elixir",
            "defmodule A do\n  def f, do: 1\nend\n",
            "defmodule A do\n  def f, do: 1\n",
        ),
        (
            "go",
            "package main\n\nfunc main() {}\n",
            "package main\n\nfunc main() {\n",
        ),
        (
            "haskell",
            "main = putStrLn \"hi\"\n",
            "main = putStrLn \"hi\n",
        ),
        (
            "hcl",
            "resource \"a\" \"b\" {\n}\n",
            "resource \"a\" \"b\" {\n",
        ),
        (
            "html",
            "<!DOCTYPE html>\n<html><body><p>hi</p></body></html>\n",
            "<html><body><p>hi\n",
        ),
        ("java", "class A {}\n", "class A {\n"),
        ("javascript", "const a = 1;\n", "function f() {\n"),
        ("json", "{\"a\": 1}\n", "{\"a\": 1\n"),
        ("kotlin", "fun main() {}\n", "fun main() {\n"),
        ("lua", "local a = 1\n", "function f(\n"),
        // Markdown's refusal surface is its table scanner:
        // `pipe_table_delimiter_row` in the block grammar requires a `|` after
        // every delimiter cell, so a header row followed by `|---` cannot
        // complete and carries a `MISSING` node. The source is otherwise valid
        // GFM, which is the point of pinning it — the fixture records what
        // *this* compiled grammar refuses, so a grammar bump that starts
        // accepting it fails this row rather than passing quietly. The valid
        // row carries a table written the way that scanner accepts it.
        (
            "markdown",
            "# Title\n\n| a | b |\n|---|---|\n| 1 | 2 |\n",
            "| a |\n|---\n| b |\n",
        ),
        ("nix", "{ pkgs }: pkgs.hello\n", "let a = 1;\n"),
        ("php", "<?php echo \"hi\";\n", "<?php function f() {\n"),
        (
            "python",
            "def f():\n    return 1\n",
            "def f(:\n    return 1\n",
        ),
        ("ruby", "def f\n  1\nend\n", "def f\n  1\n"),
        ("rust", "fn main() {}\n", "fn main( {\n"),
        (
            "scala",
            "object A { def f = 1 }\n",
            "object A { def f = 1\n",
        ),
        ("solidity", "contract A {}\n", "contract A {\n"),
        ("swift", "func f() {}\n", "func f() {\n"),
        ("tsx", "const A = () => <div />;\n", "function f() {\n"),
        ("typescript", "const a: number = 1;\n", "function f() {\n"),
        ("yaml", "a: 1\n", "a: [1, 2\n"),
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
            let Some(&(_, valid, malformed)) = FIXTURES.iter().find(|row| row.0 == name) else {
                failures.push(format!("`{name}` is compiled but has no fixture row"));
                continue;
            };
            if let Err(refusal) = try_parse(valid, language) {
                failures.push(format!("`{name}` refused its valid fixture: {refusal}"));
            }
            match try_parse(malformed, language) {
                Err(ParseError::InvalidSyntax { .. }) => {}
                Ok(_) => failures.push(format!(
                    "`{name}` accepted a malformed fixture: {malformed:?}"
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
        for &(name, _, _) in FIXTURES {
            assert!(!covered.contains(&name), "two fixture rows cover `{name}`");
            covered.push(name);
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
        for &(name, _, _) in FIXTURES {
            assert!(
                Language::ALL.iter().any(|language| language.name() == name),
                "fixture row `{name}` names no grammar that `full` compiled"
            );
        }
        assert_eq!(
            Language::ALL.len(),
            FIXTURES.len(),
            "`full` compiles every declared grammar, so every one must have a fixture row"
        );
    }
}

/// The deadline's stopping rule, on a clock the test drives.
#[cfg(all(test, feature = "lang-go"))]
mod deadline_tests {
    use super::{Duration, Instant, LanguageExt, ParseError, run_parser};

    /// A source the Go grammar needs many thousands of progress checks to parse:
    /// its own valid source tiled to the byte ceiling with the newlines removed.
    fn go_longline() -> String {
        let fragment = "package main\n\nfunc main() {}\n";
        let mut source = String::with_capacity(super::MAX_SOURCE_BYTES);
        while source.len().saturating_add(fragment.len()) <= super::MAX_SOURCE_BYTES {
            source.push_str(fragment);
        }
        source.replace('\n', "")
    }

    /// A parse is stopped at the first progress check that reads the deadline or
    /// later, never one check after, and the next parse on the same parser runs
    /// to completion.
    ///
    /// The clock moves one millisecond per reading, so a deadline of `d` ms is
    /// crossed on the `d`-th progress check: the parse must have read the clock
    /// exactly `d + 1` times, the start included. Exact, whatever the host is
    /// doing, which is what a bound on the host's own clock could not be.
    #[test]
    fn a_parse_stops_at_the_first_check_past_its_deadline() -> Result<(), String> {
        let grammar: super::TSLanguage = super::SupportLang::Go.get_ts_language();
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&grammar)
            .map_err(|error| error.to_string())?;
        let source = go_longline();
        let origin = Instant::now();
        for millis in [1_u64, 25, 50, 100, 1_000] {
            let mut readings = 0_u64;
            let mut now = origin;
            let mut overflowed = false;
            let mut clock = || {
                let reading = now;
                match now.checked_add(Duration::from_millis(1)) {
                    Some(next) => now = next,
                    None => overflowed = true,
                }
                readings = readings.saturating_add(1);
                reading
            };
            let deadline = Duration::from_millis(millis);
            let outcome = run_parser(&mut parser, &source, "go", deadline, &mut clock);
            match outcome {
                Err(ParseError::TimedOut { language, after }) => {
                    assert_eq!(language, "go", "the refusal names the grammar");
                    assert_eq!(after, deadline, "and the deadline it applied");
                }
                other => {
                    return Err(format!(
                        "{deadline:?}: a parse longer than its deadline must be stopped, got {:?}",
                        other.err()
                    ));
                }
            }
            assert!(
                !overflowed,
                "the test clock ran past what an Instant can hold"
            );
            assert_eq!(
                readings,
                millis.saturating_add(1),
                "a {millis} ms deadline must stop the parse on its {millis}th check, \
                 not before and not after"
            );
            let next = run_parser(
                &mut parser,
                "package main\n\nfunc main() {}\n",
                "go",
                Duration::MAX,
                &mut Instant::now,
            );
            assert!(
                next.is_ok(),
                "the parser must start the next source clean after a stop, got {:?}",
                next.err()
            );
        }
        Ok(())
    }
}
