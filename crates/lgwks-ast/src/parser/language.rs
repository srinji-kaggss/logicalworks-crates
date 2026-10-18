//! The grammar table, the shebang reader and the registered-grammar type.

use ast_grep_core::Language as CoreLanguage;
use ast_grep_core::matcher::{Pattern, PatternBuilder, PatternError};
use ast_grep_core::tree_sitter::LanguageExt;

use super::{MAX_SHEBANG_BYTES, StrDoc, SupportLang, TSLanguage};

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
pub(super) fn extension_of(path: &str) -> Option<&str> {
    path.rsplit_once('.').map(|(_, extension)| extension)
}

/// Whether `extension` equals any entry of `known`, ASCII-case-insensitively.
pub(super) fn any_extension_matches(known: &[&str], extension: &str) -> bool {
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
    /// to resolve through [`try_detect_content`](crate::try_detect_content), which parses rather than reads
    /// a name. Where a shebang and an extension disagree, this returns the
    /// shebang; call [`Language::of_path`] when the filename must win.
    ///
    /// Only the first line is examined, and only when it is under
    /// [`MAX_SHEBANG_BYTES`]. A file whose first line is longer than that is
    /// refused rather than scanned, so a binary presented as text costs a
    /// bounded read.
    ///
    /// `rustc`, an interpreter this table does not claim, `#!/bin/sh` and a source with
    /// no shebang at all each name no language; the unit tests
    /// `a_shebang_names_the_language_of_an_extensionless_file` and
    /// `an_unknown_interpreter_names_no_language` run those cases.
    #[must_use]
    pub fn of_shebang(source: &str) -> Option<Self> {
        let first = match source.split_once('\n') {
            Some((line, _)) => line,
            None => source,
        };
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
/// [`try_parse_with`](crate::try_parse_with) and every bounded walker.
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
