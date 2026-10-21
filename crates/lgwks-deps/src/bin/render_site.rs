//! Renders the project page, `robots.txt` and `sitemap.xml` into a directory.
//!
//! The page is the one address whose title is the repository's exact name, and
//! it answers the question a reader arrives with: what this is, when to use it,
//! and when not to. It is generated rather than checked in because the
//! repository holds Rust only, and because a page built from the same crate list
//! as the README cannot drift from it: a test walks `crates/` and fails when a
//! crate is missing here.
//!
//! Usage: `render_site <DIRECTORY>` (default: `target/site`).

use std::fmt::Write as FmtWrite;
use std::fmt::{self, Display, Formatter};
use std::fs;
use std::io::{self, Write as IoWrite};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// Where the page is served from, with its trailing slash.
const SITE_URL: &str = "https://srinji-kaggss.github.io/logicalworks-crates/";
/// The repository the page describes.
const REPO_URL: &str = "https://github.com/srinji-kaggss/logicalworks-crates";
/// The page title: the exact name, then what it is.
const TITLE: &str = "logicalworks-crates — the Rust crates behind Logical Works";
/// The description a result snippet uses, under 160 characters.
const DESCRIPTION: &str = "logicalworks-crates is the Rust repository of Logical Works: lgwks_std, lgwks_bot, lgwks_ast, lgwks_macros and lgwks_deps, five independently versioned crates, each usable alone.";

/// One crate as the page presents it.
struct Crate {
    /// The package name, underscore spelling.
    name: &'static str,
    /// What it gives a reader, in one line.
    summary: &'static str,
}

/// The crates this repository publishes.
const CRATES: [Crate; 5] = [
    Crate {
        name: "lgwks_std",
        summary: "JSON, RON and wire codecs, blocking HTTP, retry policies, time, ids, hashing, globs and structured logging — no async runtime required",
    },
    Crate {
        name: "lgwks_bot",
        summary: "Orchestration you cannot write wrongly: script! flows with bounded fan-out, deadlines, retries under one budget, tenant-keyed steps, and a capability proof checked before every verb",
    },
    Crate {
        name: "lgwks_ast",
        summary: "One AST type and one diagnostic type across several languages, bounded traversal, byte-accurate spans, and grammars behind cargo features",
    },
    Crate {
        name: "lgwks_macros",
        summary: "The syntax of script!, reached through lgwks_bot rather than depended on directly",
    },
    Crate {
        name: "lgwks_deps",
        summary: "Dependency admission, audit and a storefront: every external edge names its owner, and lgwks-deps check refuses an unowned one",
    },
];

/// Why the page could not be written.
#[derive(Debug)]
enum Error {
    /// A directory or file could not be written.
    Io {
        /// What was being written.
        path: PathBuf,
        /// What the operating system said.
        cause: io::Error,
    },
    /// A formatted write failed; rendering into a `String` should not fail.
    Render {
        /// Why the formatter refused.
        cause: fmt::Error,
    },
}

impl Display for Error {
    /// Names the path and the operating system's reason.
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Io {
                ref path,
                ref cause,
            } => write!(formatter, "cannot write {}: {cause}", path.display()),
            Self::Render { ref cause } => write!(formatter, "rendering failed: {cause}"),
        }
    }
}

impl std::error::Error for Error {
    /// The formatter's own error, so a caller can see why rendering stopped.
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match *self {
            Self::Render { ref cause } => Some(cause),
            Self::Io { .. } => None,
        }
    }
}

/// Writes `text` to `path`, naming the path when the write fails.
fn write_file(path: &Path, text: &str) -> Result<(), Error> {
    fs::write(path, text).map_err(|cause| Error::Io {
        path: path.to_path_buf(),
        cause,
    })
}

/// The crate rows of the page, one per published crate.
fn crate_rows(html: &mut String) -> Result<(), Error> {
    for item in &CRATES {
        writeln!(
            html,
            "<tr><td><a href=\"https://docs.rs/{0}\">{0}</a></td><td>{1}</td></tr>",
            item.name, item.summary
        )
        .map_err(|cause| Error::Render { cause })?;
    }
    Ok(())
}

/// The `SoftwareSourceCode` entity and its publisher, as JSON-LD.
fn entity_graph() -> String {
    let addresses: Vec<String> = CRATES
        .iter()
        .map(|item| format!("\"https://crates.io/crates/{}\"", item.name))
        .collect();
    let trimmed = addresses.join(", ");
    format!(
        r#"{{
  "@context": "https://schema.org",
  "@graph": [
    {{
      "@type": "Organization",
      "@id": "{SITE_URL}#publisher",
      "name": "Logical Works Incorporated",
      "url": "https://github.com/srinji-kaggss"
    }},
    {{
      "@type": "SoftwareSourceCode",
      "@id": "{SITE_URL}#project",
      "name": "logicalworks-crates",
      "alternateName": ["logicalworks crates", "lgwks", "lgwks_std", "lgwks_bot", "lgwks_ast", "lgwks_macros", "lgwks_deps"],
      "description": "{DESCRIPTION}",
      "codeRepository": "{REPO_URL}",
      "url": "{SITE_URL}",
      "programmingLanguage": "Rust",
      "license": "https://www.apache.org/licenses/LICENSE-2.0",
      "author": {{ "@id": "{SITE_URL}#publisher" }},
      "publisher": {{ "@id": "{SITE_URL}#publisher" }},
      "sameAs": [{trimmed}]
    }}
  ]
}}"#
    )
}

/// The whole page.
fn render_index() -> Result<String, Error> {
    let mut html = String::new();
    html.push_str("<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n");
    writeln!(html, "<title>{TITLE}</title>").map_err(|cause| Error::Render { cause })?;
    writeln!(
        html,
        "<meta name=\"description\" content=\"{DESCRIPTION}\">"
    )
    .map_err(|cause| Error::Render { cause })?;
    writeln!(html, "<link rel=\"canonical\" href=\"{SITE_URL}\">")
        .map_err(|cause| Error::Render { cause })?;
    html.push_str("<style>\n:root{color-scheme:light dark;--ink:#111;--dim:#555;--line:#d5d5d5;--bg:#fff}\n@media(prefers-color-scheme:dark){:root{--ink:#eee;--dim:#aaa;--line:#333;--bg:#111}}\nbody{margin:0 auto;max-width:46rem;padding:3rem 1.25rem 4rem;background:var(--bg);color:var(--ink);font:16px/1.6 ui-sans-serif,-apple-system,\"Segoe UI\",Roboto,sans-serif}\nh1{font-size:2.1rem;margin:0 0 .25rem;letter-spacing:-.02em}\nh2{font-size:1.15rem;margin:2.25rem 0 .5rem}\np.lede{color:var(--dim);margin:0 0 2rem;font-size:1.05rem}\ncode,pre{font-family:ui-monospace,SFMono-Regular,Menlo,monospace;font-size:.92em}\npre{background:#8881;padding:.9rem 1rem;overflow-x:auto;border-radius:6px}\ntable{border-collapse:collapse;width:100%;margin:.5rem 0 0}\nth,td{text-align:left;vertical-align:top;padding:.45rem .5rem;border-bottom:1px solid var(--line)}\nth{color:var(--dim);font-weight:600}\n.grid{display:grid;gap:1.5rem;grid-template-columns:1fr 1fr}\n@media(max-width:40rem){.grid{grid-template-columns:1fr}}\nul{padding-left:1.1rem}li{margin:.3rem 0}\nfooter{margin-top:3rem;padding-top:1rem;border-top:1px solid var(--line);color:var(--dim);font-size:.9rem}\na{color:inherit}\n</style>\n");
    html.push_str("<script type=\"application/ld+json\">\n");
    html.push_str(&entity_graph());
    html.push_str("\n</script>\n</head>\n<body>\n<h1>logicalworks-crates</h1>\n");
    html.push_str("<p class=\"lede\">The Rust crate estate behind Logical Works. Five independently versioned crates, each usable on its own.</p>\n");
    html.push_str("<p><strong>logicalworks-crates</strong> is the Rust repository of Logical Works Incorporated. It is not a framework and not a rewrite of the ecosystem: it is the layer where a program's authority, its dependency graph and its reading of source code are decided once, and enforced by a compiler and a gate rather than by review.</p>\n");
    html.push_str("<h2>The five crates</h2>\n<table>\n<thead><tr><th>Crate</th><th>What it gives you</th></tr></thead>\n<tbody>\n");
    crate_rows(&mut html)?;
    html.push_str("</tbody>\n</table>\n<h2>Use it, or don't</h2>\n<div class=\"grid\">\n<div>\n<h3>Use it when</h3>\n<ul>\n");
    html.push_str(
        "<li>you want bounded, cancellable automation whose authority is a checked value;</li>\n",
    );
    html.push_str("<li>you need HTTP and JSON from a CLI without an async runtime;</li>\n");
    html.push_str(
        "<li>you want one AST across several languages, compiling only what you ship;</li>\n",
    );
    html.push_str("<li>you want \"what is in this build, and who approved it?\" answered by a command.</li>\n");
    html.push_str("</ul>\n</div>\n<div>\n<h3>Don't use it when</h3>\n<ul>\n");
    html.push_str("<li>you need a battle-tested default with years of production use — use <code>serde</code>, <code>reqwest</code>, <code>tokio</code>, <code>tracing</code>;</li>\n");
    html.push_str("<li>you need an SLA, an LTS branch or paid support;</li>\n");
    html.push_str(
        "<li>you want an agent framework, an async runtime, or a distributed system;</li>\n",
    );
    html.push_str(
        "<li>file-level copyleft is a problem (<code>lgwks_bot</code> is MPL-2.0).</li>\n",
    );
    html.push_str("</ul>\n</div>\n</div>\n<h2>Start</h2>\n<pre><code>cargo add lgwks_std --features json,http\ncargo add lgwks_bot\ncargo install lgwks_deps &amp;&amp; lgwks-deps init . &amp;&amp; lgwks-deps check .</code></pre>\n");
    writeln!(html, "<h2>Read more</h2>\n<ul>\n<li><a href=\"{REPO_URL}/blob/main/docs/should-i-use.md\">Should I use logicalworks-crates?</a> — the honest decision guide, including where each crate is the wrong choice.</li>").map_err(|cause| Error::Render { cause })?;
    writeln!(
        html,
        "<li><a href=\"{REPO_URL}\">Source, tests and gates</a> on GitHub.</li>"
    )
    .map_err(|cause| Error::Render { cause })?;
    writeln!(html, "<li><a href=\"{REPO_URL}/blob/main/llms.txt\">llms.txt</a> — one fetch for an agent that needs the index.</li>\n</ul>").map_err(|cause| Error::Render { cause })?;
    html.push_str("<footer>Published by Logical Works Incorporated. <code>lgwks_std</code>, <code>lgwks_ast</code> and <code>lgwks_deps</code> are Apache-2.0; <code>lgwks_bot</code> is MPL-2.0. This page is not affiliated with LogicWorks circuit-design software.</footer>\n</body>\n</html>\n");
    Ok(html)
}

/// `robots.txt`, naming the sitemap.
fn render_robots() -> String {
    format!("User-agent: *\nAllow: /\n\nSitemap: {SITE_URL}sitemap.xml\n")
}

/// `sitemap.xml` with the one page.
fn render_sitemap() -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<urlset xmlns=\"http://www.sitemaps.org/schemas/sitemap/0.9\">\n  <url>\n    <loc>{SITE_URL}</loc>\n    <changefreq>weekly</changefreq>\n    <priority>1.0</priority>\n  </url>\n</urlset>\n"
    )
}

/// Renders the three files into `directory`, creating it when absent.
fn run(directory: &Path, out: &mut impl IoWrite) -> Result<(), Error> {
    fs::create_dir_all(directory).map_err(|cause| Error::Io {
        path: directory.to_path_buf(),
        cause,
    })?;
    write_file(&directory.join("index.html"), &render_index()?)?;
    write_file(&directory.join("robots.txt"), &render_robots())?;
    write_file(&directory.join("sitemap.xml"), &render_sitemap())?;
    writeln!(
        out,
        "wrote {} — index.html, robots.txt, sitemap.xml, {} crates",
        directory.display(),
        CRATES.len()
    )
    .map_err(|cause| Error::Io {
        path: PathBuf::from("<stdout>"),
        cause,
    })?;
    Ok(())
}

/// The directory named on the command line, else `target/site`.
fn directory_argument() -> PathBuf {
    match std::env::args_os().nth(1) {
        Some(argument) => PathBuf::from(argument),
        None => PathBuf::from("target/site"),
    }
}

/// Renders the page and reports what it wrote.
fn main() -> ExitCode {
    let stdout = io::stdout();
    let mut out = stdout.lock();
    let stderr = io::stderr();
    let mut err = stderr.lock();
    match run(&directory_argument(), &mut out) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            if writeln!(err, "error: {error}").is_err() {
                return ExitCode::from(2);
            }
            ExitCode::from(1)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CRATES, DESCRIPTION, SITE_URL, TITLE, render_index, render_robots, render_sitemap,
    };
    use std::error::Error;
    use std::fs;
    use std::path::Path;

    type TestResult = Result<(), Box<dyn Error>>;

    /// Every crate directory is on the page, so a sixth crate cannot ship
    /// without appearing here.
    #[test]
    fn every_crate_directory_is_on_the_page() -> TestResult {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let rendered = render_index()?;
        for entry in fs::read_dir(root.join("crates"))? {
            let name = entry?.file_name().to_string_lossy().into_owned();
            let package = name.replace('-', "_");
            assert!(
                rendered.contains(&package),
                "the page names no crate for crates/{name}"
            );
        }
        Ok(())
    }

    /// The title, description and canonical address are the ones a crawler and
    /// an answer engine read.
    #[test]
    fn the_head_names_the_page_and_its_entity() -> TestResult {
        let rendered = render_index()?;
        assert!(rendered.contains(TITLE), "the title is missing");
        assert!(rendered.contains(DESCRIPTION), "the description is missing");
        let canonical = format!("<link rel=\"canonical\" href=\"{SITE_URL}\">");
        assert!(
            rendered.contains(&canonical),
            "the canonical link is missing"
        );
        assert!(
            rendered.contains("SoftwareSourceCode"),
            "the entity graph is missing"
        );
        assert!(
            rendered.contains("Logical Works Incorporated"),
            "the publisher is missing"
        );
        for item in &CRATES {
            assert!(
                rendered.contains(item.name),
                "the page does not name {}",
                item.name
            );
        }
        Ok(())
    }

    /// The crawl files point at the same address as the page.
    #[test]
    fn the_crawl_files_name_the_site() {
        let sitemap_line = format!("Sitemap: {SITE_URL}sitemap.xml");
        assert!(render_robots().contains(&sitemap_line));
        assert!(render_sitemap().contains(SITE_URL));
    }
}
