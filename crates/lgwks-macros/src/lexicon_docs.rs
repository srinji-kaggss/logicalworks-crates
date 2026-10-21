//! The pages generated from the lexicon, checked against the lexicon.
//!
//! The lexicon itself lives with the parser in `lgwks_ast::script` (#383); the
//! crate documentation and the README that describe the language live here,
//! beside the macro a reader reaches them through. Each table is rendered from
//! [`LEXICON`] and must appear verbatim, and every guarantee a word makes must
//! name a test that exists.

use std::collections::HashSet;
use std::path::PathBuf;

use lgwks_ast::script::{Evidence, Kind, LEXICON, Position, Word};

use crate::tests::{REFUSALS, expand};

/// The workspace root, which every path a test names is relative to.
fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The block table, one row per written form, every line starting with
/// `prefix`.
fn table(prefix: &str) -> String {
    let rows = LEXICON
        .iter()
        .flat_map(Word::forms)
        .map(|form| format!("{prefix}| {} | {} |\n", form.written(), form.means()));
    [
        format!("{prefix}| Written | Means |\n"),
        format!("{prefix}|---|---|\n"),
    ]
    .into_iter()
    .chain(rows)
    .collect()
}

/// The crate documentation's block table, as `//!` lines.
fn doc_table() -> String {
    table("//! ")
}

/// The first word's example, indented as the body of a `lgwks_bot::script!`
/// block.
fn wrapped_example() -> String {
    LEXICON
        .first()
        .map(Word::example)
        .into_iter()
        .flat_map(str::lines)
        .map(|line| format!("    {line}\n"))
        .collect()
}

/// The README's word table: every row, with the columns the crate
/// documentation leaves to the README.
fn readme_table() -> Result<String, String> {
    let mut lines = Vec::with_capacity(LEXICON.len());
    for row in &LEXICON {
        let written: Vec<&str> = row.forms().iter().map(|form| form.written()).collect();
        let means: Vec<String> = row.forms().iter().map(|form| plain(form.means())).collect();
        let written = if written.is_empty() {
            format!("`{}` (see `if`)", row.kind().spelling())
        } else {
            written.join("<br>")
        };
        lines.push(format!(
            "| {written} | {} | {} | {} | {} | {} |\n",
            row.position().name(),
            row.primitive(),
            cell(&means.join("<br>")),
            cell(&row.refuses().join("; ")),
            cell(&guarantees(row)?),
        ));
    }
    Ok(format!(
        "| Written | Where | Calls | Means | Refuses | Guarantees |\n|---|---|---|---|---|---|\n{}",
        lines.concat()
    ))
}

/// A row's guarantees as one README cell: each claim with its axis and proof.
fn guarantees(row: &Word) -> Result<String, String> {
    let mut cited = Vec::with_capacity(row.guarantees().len());
    for guarantee in row.guarantees() {
        cited.push(format!(
            "{} ({}; {})",
            guarantee.claim(),
            guarantee.axis().name(),
            cite(guarantee.evidence())?
        ));
    }
    Ok(cited.join("; "))
}

/// The evidence as the README cites it.
fn cite(evidence: &Evidence) -> Result<String, String> {
    match *evidence {
        Evidence::Refusal(case) => Ok(format!("refusal *{case}*")),
        // A path with no directory is already the file's own name.
        Evidence::Sim { file, test } => Ok(match file.rsplit_once('/') {
            Some((_, name)) => format!("`{test}` in `{name}`"),
            None => format!("`{test}` in `{file}`"),
        }),
        // `Evidence` is `#[non_exhaustive]`: a kind of proof a newer
        // `lgwks_ast` adds has no citation form here until one is written.
        _ => Err(format!("no citation form for {evidence:?}")),
    }
}

/// An intra-doc link rendered as the code it names, for a page rustdoc does
/// not resolve.
fn plain(text: &str) -> String {
    text.replace("[`", "`").replace("`]", "`")
}

/// An empty cell written as a dash, so a reader sees "nothing" rather than a
/// gap.
fn cell(text: &str) -> &str {
    if text.is_empty() { "—" } else { text }
}

/// The crate documentation's block table is the one the lexicon renders.
#[test]
fn the_crate_doc_table_is_rendered_from_the_lexicon() {
    let table = doc_table();
    assert!(
        include_str!("lib.rs").contains(&table),
        "src/lib.rs must carry the lexicon's block table verbatim:\n{table}"
    );
}

/// The README's word table is the one the lexicon renders.
#[test]
fn the_readme_table_is_rendered_from_the_lexicon() -> Result<(), String> {
    let table = readme_table()?;
    assert!(
        include_str!("../README.md").contains(&table),
        "README.md must carry the lexicon's word table verbatim:\n{table}"
    );
    Ok(())
}

/// One row per spelling: two rows for one token would make dispatch depend on
/// table order.
#[test]
fn every_spelling_has_exactly_one_row() {
    let mut seen = HashSet::new();
    for row in &LEXICON {
        assert!(
            seen.insert(row.kind().spelling()),
            "`{}` has two rows",
            row.kind().spelling()
        );
    }
}

/// Every row's example is a script the macro expands, and it uses the word it
/// illustrates.
#[test]
fn every_example_expands_and_uses_its_word() -> Result<(), String> {
    for row in &LEXICON {
        let spelling = row.kind().spelling();
        let uses_word = row
            .example()
            .split(|character: char| !character.is_ascii_alphanumeric() && character != '_')
            .any(|token| token == spelling);
        assert!(uses_word, "the example for `{spelling}` never says it");
        expand(row.example())
            .map_err(|refusal| format!("the example for `{spelling}` is refused: {refusal}"))?;
    }
    Ok(())
}

/// Every word guarantees something, and every guarantee names a test that
/// exists: a compile-fail case in `REFUSALS`, or a `#[test]` or swept family in
/// a `sim_` file. Renaming or deleting the test fails this, so a guarantee
/// cannot outlive its proof. The tests themselves run in the same gate, so
/// existing is what is left to check here.
#[test]
fn every_guarantee_names_a_test_that_exists() -> Result<(), String> {
    for row in &LEXICON {
        let spelling = row.kind().spelling();
        if row.guarantees().is_empty() {
            return Err(format!("`{spelling}` guarantees nothing"));
        }
        for guarantee in row.guarantees() {
            resolve(guarantee.evidence())
                .map_err(|why| format!("`{spelling}` ({}): {why}", guarantee.claim()))?;
        }
    }
    Ok(())
}

/// Find the test `evidence` names.
fn resolve(evidence: &Evidence) -> Result<(), String> {
    match *evidence {
        Evidence::Refusal(case) => {
            if REFUSALS.iter().any(|&(name, _, _)| name == case) {
                Ok(())
            } else {
                Err(format!("no compile-fail case named {case:?}"))
            }
        }
        Evidence::Sim { file, test } => {
            let is_sim = file
                .rsplit_once('/')
                .is_some_and(|(_, name)| name.starts_with("sim_"));
            if !is_sim {
                let refusal = format!("{file} is not a simulation file");
                lgwks_std::trace::debug!(error = %refusal, "resolve: returning an error to the caller");
                return Err(refusal);
            }
            let source = std::fs::read_to_string(workspace_root().join(file))
                .map_err(|error| format!("{file}: {error}"))?;
            if defines_test(&source, test) {
                Ok(())
            } else {
                Err(format!("{file} has no test or swept family `{test}`"))
            }
        }
        _ => Err(format!("no resolver for {evidence:?}")),
    }
}

/// Whether `source` declares `test` as a `#[test]` function, or defines it as
/// a family a `band_family!` sweeps.
fn defines_test(source: &str, test: &str) -> bool {
    let header = format!("fn {test}(");
    let lines: Vec<&str> = source.lines().map(str::trim_start).collect();
    let Some(at) = lines.iter().position(|line| line.starts_with(&header)) else {
        return false;
    };
    let attributed = lines
        .iter()
        .take(at)
        .rev()
        .take_while(|line| line.starts_with("#[") || line.starts_with("///"))
        .any(|line| line.starts_with("#[test]"));
    attributed || source.contains(&format!("=> {test}, "))
}

/// The resolver is not vacuous: a name with no test behind it, a swept family
/// that is not swept, and a case not in the table are all refused.
#[test]
fn a_guarantee_whose_test_is_gone_is_refused() {
    let sweeps = "fn fan(band: Band) -> TestResult {}\nband_family! { fan_band_00 => fan, 0; }\n";
    assert!(defines_test(sweeps, "fan"), "a swept family resolves");
    assert!(
        !defines_test(sweeps, "fan_gone"),
        "a missing family does not"
    );
    let unswept = "fn helper(band: Band) -> TestResult {}\n";
    assert!(
        !defines_test(unswept, "helper"),
        "an unswept helper does not"
    );
    let attributed = "/// Doc.\n#[test]\nfn proves() {}\n";
    assert!(defines_test(attributed, "proves"), "a `#[test]` resolves");
    assert!(
        resolve(&Evidence::Refusal("no such case")).is_err(),
        "an unknown compile-fail case does not"
    );
    assert!(
        resolve(&Evidence::Sim {
            file: "crates/lgwks-bot/tests/it/script_flow.rs",
            test: "a_deadline_that_passes_fails_the_step_as_timed_out",
        })
        .is_err(),
        "a test outside a simulation file is not evidence"
    );
}

/// The lexicon's budget: what an agent can hold whole beside its task, over
/// every page together.
const PAGE_TOKENS: usize = 4_000;

/// The most lines one committed page holds: the estate's size law for a
/// repository `.md` file (Director 2026-10-10).
const PAGE_LINES: usize = 120;

/// The variable that makes the page test write the pages it renders.
const WRITE_PAGE: &str = "LGWKS_WRITE_LEXICON_PAGE";

/// Page `number`'s file name under `docs/`. The first keeps the name the
/// lexicon has always had, so every link to it still lands on its start.
fn page_file(number: usize) -> String {
    if number <= 1 {
        String::from("script-lexicon.md")
    } else {
        format!("script-lexicon-{number}.md")
    }
}

/// Page `number`'s path from the workspace root.
fn page_path(number: usize) -> String {
    format!("docs/{}", page_file(number))
}

/// How many lines `text` holds, counted as a file's newlines.
fn lines(text: &str) -> usize {
    text.matches('\n').count()
}

/// Refuse with `why`, logged first.
fn refuse<T>(why: String) -> Result<T, String> {
    lgwks_std::trace::debug!(error = %why, "lexicon pages: refused");
    Err(why)
}

/// What every page opens with: its title, its generator and the line linking
/// every page, `number` of `total` named and not linked.
fn page_header(number: usize, total: usize) -> String {
    let title = if number <= 1 {
        String::new()
    } else {
        format!(", page {number}")
    };
    let links: Vec<String> = (1..=total)
        .map(|other| {
            if other == number {
                format!("page {other}")
            } else {
                format!("[page {other}]({})", page_file(other))
            }
        })
        .collect();
    format!(
        "# The `script!` lexicon{title}\n\n\
         <!-- Generated from `LEXICON` in crates/lgwks-ast/src/script/lexicon.rs by the \
         test `the_lexicon_page_is_rendered_from_the_lexicon` in lgwks_macros. Edit the \
         table, not this page. -->\n\n\
         Pages: {}.\n",
        links.join(" · ")
    )
}

/// What the first page says before the words.
fn intro() -> String {
    format!(
        "\nEverything needed to write `lgwks_bot::script!` flows. A script is one or \
         more `flow`s inside the macro:\n\n\
         ```rust\nlgwks_bot::script! {{\n{}}}\n```\n\n\
         Each line is one step: it starts with one of the words below, or it is a \
         line of Rust. A line ending in `:` opens a block of the lines indented four \
         spaces beneath it. Inside a flow, `scope` is the current step's scope. A \
         line of Rust that could end or leak the program is refused when the macro \
         expands, and the refusal names what to write instead.\n",
        wrapped_example()
    )
}

/// The lexicon (#385), an agent's whole context for writing a script: each
/// word's forms, what it refuses, what it guarantees and an example, rendered
/// from [`LEXICON`] so it cannot say what the parser does not do. Proofs are
/// left to the README's table; the pages carry what an author needs, inside
/// the token budget.
///
/// Words fill pages in table order, a word never split, each page at most
/// [`PAGE_LINES`] lines with its header, so the lexicon can grow without a page
/// outgrowing the size law.
fn pages() -> Result<Vec<String>, String> {
    let header_lines = lines(&page_header(1, 1));
    let mut bodies = vec![intro()];
    for row in &LEXICON {
        let section = section(row)?;
        let fits = |body: &str| {
            header_lines
                .saturating_add(lines(body))
                .saturating_add(lines(&section))
                <= PAGE_LINES
        };
        match bodies.last_mut() {
            Some(body) if fits(body) => body.push_str(&section),
            _ if !fits("") => {
                return refuse(format!(
                    "the `{}` section alone is past {PAGE_LINES} lines",
                    heading(row.kind())
                ));
            }
            _ => bodies.push(section),
        }
    }
    let total = bodies.len();
    Ok(bodies
        .iter()
        .enumerate()
        .map(|(index, body)| format!("{}{body}", page_header(index.saturating_add(1), total)))
        .collect())
}

/// One word's section of the page.
fn section(row: &Word) -> Result<String, String> {
    let forms: String = row
        .forms()
        .iter()
        .map(|form| format!("- {} — {}\n", form.written(), plain(form.means())))
        .collect();
    let forms = if forms.is_empty() {
        "Written as part of `if`'s forms.\n".to_owned()
    } else {
        forms
    };
    let refuses = if row.refuses().is_empty() {
        "nothing of its own".to_owned()
    } else {
        row.refuses().join("; ")
    };
    let guarantees: Vec<String> = row
        .guarantees()
        .iter()
        .map(|guarantee| format!("{} ({})", guarantee.claim(), guarantee.axis().name()))
        .collect();
    let example = if row.kind() == Kind::Else {
        String::new()
    } else {
        format!("\n```text\n{}```\n", row.example())
    };
    Ok(format!(
        "\n## `{}`\n\n{}. Calls {}.\n\n{forms}\nRefuses: {refuses}.\n\nGuarantees: {}.\n{example}",
        heading(row.kind()),
        stands(row.position())?,
        row.primitive(),
        guarantees.join("; "),
    ))
}

/// The word as an author says it: `give back` is two tokens.
fn heading(kind: Kind) -> &'static str {
    if kind == Kind::GiveBack {
        "give back"
    } else {
        kind.spelling()
    }
}

/// Where a word stands, as a sentence.
fn stands(position: Position) -> Result<&'static str, String> {
    match position {
        Position::Flow => Ok("Opens a flow at the top level"),
        Position::Block => Ok("Opens a block: its line ends in `:`"),
        Position::Line => Ok("Starts a line of its own"),
        Position::Inline => Ok("Stands inside a line of Rust"),
        // `Position` is `#[non_exhaustive]`: a place a newer `lgwks_ast` adds
        // has no sentence here until one is written.
        _ => Err(format!("no sentence for {position:?}")),
    }
}

/// The committed pages are the ones the table renders, none past the size
/// law, no page left over from a longer lexicon, and all of them together
/// inside the budget. Tokens are counted as bytes divided by four, rounded up:
/// a tokenizer-free bound that overcounts English prose, so pages under it are
/// under it for any common tokenizer. Set `LGWKS_WRITE_LEXICON_PAGE` to write
/// the rendered pages instead of failing on stale ones.
#[test]
fn the_lexicon_page_is_rendered_from_the_lexicon() -> Result<(), String> {
    let rendered = pages()?;
    let writing = std::env::var_os(WRITE_PAGE).is_some();
    let mut bytes: usize = 0;
    for (index, page) in rendered.iter().enumerate() {
        let name = page_path(index.saturating_add(1));
        let path = workspace_root().join(&name);
        if writing {
            std::fs::write(&path, page).map_err(|error| format!("{name}: {error}"))?;
        }
        let committed =
            std::fs::read_to_string(&path).map_err(|error| format!("{name}: {error}"))?;
        if committed != *page {
            return refuse(format!(
                "{name} is generated from LEXICON and differs from it; rerun this test \
                 with {WRITE_PAGE}=1 and commit the pages"
            ));
        }
        if lines(page) > PAGE_LINES {
            return refuse(format!("{name} is past {PAGE_LINES} lines"));
        }
        bytes = bytes.saturating_add(page.len());
    }
    let leftover = page_path(rendered.len().saturating_add(1));
    if workspace_root().join(&leftover).exists() {
        return refuse(format!(
            "{leftover} is a page the lexicon no longer renders; delete it"
        ));
    }
    let tokens = bytes.div_ceil(4);
    if tokens > PAGE_TOKENS {
        return refuse(format!(
            "the lexicon pages are {tokens} tokens (bytes / 4, rounded up), past {PAGE_TOKENS}"
        ));
    }
    Ok(())
}

/// Paging is not vacuous: every page fits the size law, the words appear once
/// each in table order across the pages, and every page links every other.
#[test]
fn the_lexicon_pages_hold_every_word_once_within_the_size_law() -> Result<(), String> {
    let rendered = pages()?;
    let whole: String = rendered.concat();
    let mut from = 0;
    for row in &LEXICON {
        let marker = format!("\n## `{}`\n", heading(row.kind()));
        let Some(at) = whole.get(from..).and_then(|rest| rest.find(&marker)) else {
            return refuse(format!(
                "`{}` is missing or out of order",
                heading(row.kind())
            ));
        };
        from = from.saturating_add(at).saturating_add(marker.len());
        assert_eq!(whole.matches(&marker).count(), 1, "{marker:?} appears once");
    }
    for (index, page) in rendered.iter().enumerate() {
        assert!(
            lines(page) <= PAGE_LINES,
            "page {} is {} lines",
            index + 1,
            lines(page)
        );
        for other in 1..=rendered.len() {
            if other != index.saturating_add(1) {
                assert!(
                    page.contains(&format!("({})", page_file(other))),
                    "page {} links page {other}",
                    index + 1
                );
            }
        }
    }
    Ok(())
}

/// The crates whose rustdoc front page opens with the lexicon (#526): each
/// directory under the workspace root holds the `src/lib.rs` and the generated
/// `src/lexicon.md` this test reads.
const FRONT_PAGES: [&str; 4] = [
    "crates/lgwks-std",
    "crates/lgwks-bot",
    "crates/lgwks-ast",
    "crates/lgwks-deps",
];

/// The first line of each of those `lib.rs` files: the crate documentation
/// begins with the fragment, ahead of every other line it carries.
const OPENER: &str = "#![doc = include_str!(\"lexicon.md\")]\n";

/// Where a crate page reaches the full lexicon page.
const PAGE_URL: &str =
    "https://github.com/srinji-kaggss/logicalworks-crates/blob/main/docs/script-lexicon.md";

/// What every crate's front page opens with: the way in, one example and the
/// block table, rendered from [`LEXICON`] so no page says what the parser does
/// not do.
fn front() -> String {
    format!(
        "This crate's rustdoc opens with the `script!` lexicon, the orchestration \
         language this workspace ships. Each crate page also documents its own library \
         front door; `script!` is the supported orchestration interface, not the only \
         way to use every crate.\n\n\
         ```text\nlgwks_bot::script! {{\n{}}}\n```\n\n\
         Each line is one step. It starts with one of the words below, or it is a \
         line of Rust, passed through as written. What each word refuses and \
         guarantees is on [the one-page lexicon]({PAGE_URL}).\n\n\
         {}\n\
         [`Scope`]: https://docs.rs/lgwks_bot/latest/lgwks_bot/script/struct.Scope.html\n",
        wrapped_example(),
        table(""),
    )
}

/// What is wrong with one crate's front page: a fragment that is not the
/// rendered one, or a `lib.rs` that does not begin with the opener.
fn flaws(directory: &str, committed: &str, lib: &str, rendered: &str) -> Vec<String> {
    let mut found = Vec::with_capacity(2);
    if committed != rendered {
        found.push(format!(
            "{directory}/src/lexicon.md differs from the lexicon"
        ));
    }
    if !lib.starts_with(OPENER) {
        found.push(format!(
            "{directory}/src/lib.rs does not begin with {OPENER:?}"
        ));
    }
    found
}

/// Every crate's rustdoc front page opens with the lexicon (#526): its `lib.rs`
/// begins with the line that includes the fragment, and the committed fragment
/// is the one the lexicon renders. Set `LGWKS_WRITE_LEXICON_PAGE` to write the
/// fragments instead of failing on a stale one.
#[test]
fn every_front_page_opens_with_the_lexicon() -> Result<(), String> {
    let rendered = front();
    let writing = std::env::var_os(WRITE_PAGE).is_some();
    let mut found = Vec::new();
    for directory in FRONT_PAGES {
        let src = workspace_root().join(directory).join("src");
        let fragment = src.join("lexicon.md");
        if writing {
            std::fs::write(&fragment, &rendered)
                .map_err(|error| format!("{directory}: {error}"))?;
        }
        let committed = std::fs::read_to_string(&fragment)
            .map_err(|error| format!("{directory}/src/lexicon.md: {error}"))?;
        let lib = std::fs::read_to_string(src.join("lib.rs"))
            .map_err(|error| format!("{directory}/src/lib.rs: {error}"))?;
        found.extend(flaws(directory, &committed, &lib, &rendered));
    }
    if found.is_empty() {
        return Ok(());
    }
    Err(format!(
        "{}; rerun this test with {WRITE_PAGE}=1 and commit the fragments",
        found.join("; ")
    ))
}

/// The check is not vacuous: a stale fragment and a `lib.rs` that opens with
/// anything else are each refused, and a page that matches is not.
#[test]
fn a_front_page_that_does_not_open_with_the_lexicon_is_refused() {
    let lib = format!("{OPENER}//! The rest.\n");
    assert!(
        flaws("crates/x", "page", &lib, "page").is_empty(),
        "a matching page is clean"
    );
    assert_eq!(
        flaws("crates/x", "old", &lib, "page").len(),
        1,
        "a stale fragment is one flaw"
    );
    assert_eq!(
        flaws("crates/x", "page", "//! The rest.\n", "page").len(),
        1,
        "a lib.rs without the opener is one flaw"
    );
    assert_eq!(
        flaws("crates/x", "old", "//! The rest.\n", "page").len(),
        2,
        "both flaws are reported together"
    );
}
