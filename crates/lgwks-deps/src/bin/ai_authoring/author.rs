//! The author of a solution: the prompt skeleton every arm is held to, the
//! closed-book sandbox the model runs in, and its reply read back as code and
//! token counts.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::time::Duration;

use lgwks_std::json::{self, Value};

use super::bench::Bench;
use super::children::{Children, Launch, TIMED_OUT, inherited_env};
use super::opencode::{read_events, read_failure};
use super::{BenchError, fail};

/// What one model call returned.
pub(super) struct Reply {
    /// The reply's final text.
    pub(super) text: String,
    /// Tokens the call read.
    pub(super) tokens_in: u64,
    /// Tokens the call wrote.
    pub(super) tokens_out: u64,
    /// How long the model took, in milliseconds.
    pub(super) wall_ms: u64,
    /// Everything the call printed, for the record.
    pub(super) raw: String,
}

/// Something that answers a prompt with a reply.
pub(super) trait Author {
    /// Answers `prompt`.
    fn ask(&self, prompt: &str) -> Result<Reply, BenchError>;
}

/// What a prompt is made of.
pub(super) struct Texts {
    /// The profile's preamble, or empty for no profile.
    pub(super) preamble: String,
    /// The task's prompt.
    pub(super) task: String,
    /// The arm's API sheet.
    pub(super) sheet: String,
}

/// The one prompt skeleton, with the preamble in front of it.
///
/// The preamble goes first and the framing last, so the reply contract, one
/// fenced block and nothing else, is the last instruction the model reads and
/// every profile answers under the same one.
pub(super) fn full_prompt(framing: &str, texts: &Texts) -> String {
    let mut parts = Vec::new();
    if !texts.preamble.is_empty() {
        parts.push(format!("=== ROLE ===\n\n{}", texts.preamble));
    }
    parts.push(format!("=== TASK ===\n\n{}", texts.task));
    parts.push(format!("=== API SHEET ===\n\n{}", texts.sheet));
    format!("{framing}\n\n{}\n", parts.join("\n\n"))
}

/// How many lines of compiler output a repair prompt carries.
const ERROR_LINES: usize = 80;

/// The re-prompt after a failed attempt: the original prompt, the code and the
/// first lines of the compiler's complaint. A reply with no fenced block is a
/// failed attempt too, and is told so.
pub(super) fn repair_prompt(original: &str, code: &str, errors: &str) -> String {
    if code.is_empty() {
        return format!(
            "{original}\n\n=== PREVIOUS ATTEMPT ===\n\n\
             Your previous reply contained no fenced ```rust block. Reply with \
             exactly one fenced ```rust block containing the complete `src/lib.rs`, \
             and nothing else.\n"
        );
    }
    let shown: Vec<&str> = errors.lines().take(ERROR_LINES).collect();
    format!(
        "{original}\n\n=== PREVIOUS ATTEMPT ===\n\n\
         Your previous attempt did not compile. Here is the current complete \
         `src/lib.rs`:\n\n```rust\n{code}\n```\n\n\
         The compiler reported (first {ERROR_LINES} lines):\n\n```\n{}\n```\n\n\
         Reply with exactly one fenced ```rust block containing the corrected \
         complete `src/lib.rs`, and nothing else.\n",
        shown.join("\n")
    )
}

/// Whether the text after an opening fence's backticks, up to its newline, is an
/// info string: a language word and only blanks after it. `language` narrows it
/// to one word.
fn is_info_line(line: &str, language: Option<&str>) -> bool {
    let line = match line.strip_suffix('\r') {
        Some(trimmed) => trimmed,
        None => line,
    };
    let word_end = match line
        .find(|ch: char| !(ch.is_ascii_alphanumeric() || matches!(ch, '_' | '+' | '-')))
    {
        Some(at) => at,
        None => line.len(),
    };
    let Some((word, rest)) = line.split_at_checked(word_end) else {
        return false;
    };
    rest.chars().all(|ch| ch == ' ' || ch == '\t') && language.is_none_or(|wanted| word == wanted)
}

/// The bodies of the fenced blocks in `text`, in order; only blocks of
/// `language` when one is given.
///
/// An opener is three backticks, an info string and a newline; a run of backticks
/// that is not one is stepped over one byte at a time, so a fence written with
/// four backticks still opens at its last three.
fn fenced_blocks<'text>(text: &'text str, language: Option<&str>) -> Vec<&'text str> {
    let mut found = Vec::new();
    let mut rest = text;
    while let Some(open) = rest.find("```") {
        let Some(after_open) = rest.get(open.saturating_add(3)..) else {
            break;
        };
        let Some((info, body)) = after_open.split_once('\n') else {
            break;
        };
        if !is_info_line(info, language) {
            let Some(next) = rest.get(open.saturating_add(1)..) else {
                break;
            };
            rest = next;
            continue;
        }
        let Some((block, after_close)) = body.split_once("```") else {
            break;
        };
        found.push(block);
        rest = after_close;
    }
    found
}

/// The last ```` ```rust ```` block of a reply, else the last block of any
/// language, else nothing; always ending in one newline.
pub(super) fn extract_rust_block(text: &str) -> String {
    let mut blocks = fenced_blocks(text, Some("rust"));
    if blocks.is_empty() {
        blocks = fenced_blocks(text, None);
    }
    match blocks.last() {
        Some(block) => format!("{}\n", block.trim_end()),
        None => String::new(),
    }
}

/// A whole-number field of `holder`, or zero when it is absent.
pub(super) fn count_of(holder: Option<&Value>, key: &str) -> u64 {
    holder
        .and_then(|value| value.get(key))
        .and_then(Value::as_u64)
        .into_iter()
        .sum()
}

/// The reply a model command's final JSON result line carries, read from its
/// output: the text, the token counts and the model's own wall time. A command
/// that prints an event stream instead is read by `read_events`.
pub(super) fn read_reply(raw: &str) -> Option<Reply> {
    for line in raw.lines().rev() {
        let trimmed = line.trim();
        if !trimmed.starts_with('{') {
            continue;
        }
        let Ok(record) = json::from_str::<Value>(trimmed) else {
            continue;
        };
        let Some(object) = record.as_object() else {
            continue;
        };
        let is_result = object.contains_key("finalText")
            || object.get("type").and_then(Value::as_str) == Some("result");
        if !is_result {
            continue;
        }
        let text = match object.get("finalText").and_then(Value::as_str) {
            Some(found) => found.to_owned(),
            None => String::new(),
        };
        let usage = object.get("usage");
        return Some(Reply {
            text,
            tokens_in: count_of(usage, "inputTokens"),
            tokens_out: count_of(usage, "outputTokens"),
            wall_ms: count_of(Some(&record), "durationMs"),
            raw: String::new(),
        });
    }
    read_events(raw)
}

/// A path as an SBPL string literal.
fn sbpl_string(path: &Path) -> String {
    let shown = path.display().to_string();
    format!("\"{}\"", shown.replace('\\', "\\\\").replace('"', "\\\""))
}

/// The resolved paths a closed-book profile names.
pub(super) struct Closure {
    /// The home directory.
    pub(super) home: PathBuf,
    /// The run's work root.
    pub(super) work: PathBuf,
    /// This trial's scratch directory.
    pub(super) scratch: PathBuf,
}

/// `(subpath ..)` clauses for each literal in `paths`.
fn subpaths(paths: &[String]) -> String {
    paths
        .iter()
        .map(|path| format!("(subpath {path})"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// The `sandbox-exec` profile that makes a trial closed-book.
///
/// The model command runs with tools, so without this it can read the hidden
/// oracle, the reference solutions, the crate source, earlier sessions and the
/// sibling trials' passing answers; the first matrix did exactly that. The
/// profile denies the home directory, the named system directories and the
/// run's work root, then lets back in only the command's own settings and this
/// trial's scratch directory. Paths are resolved, because the kernel matches the
/// real path, never the spelling in `$TMPDIR`.
pub(super) fn closed_book_profile(bench: &Bench, closure: &Closure) -> String {
    let config = closure.home.join(&bench.model.config_dir);
    let mut denied = vec![sbpl_string(&closure.home)];
    denied.extend(
        bench
            .model
            .denied
            .iter()
            .map(|path| sbpl_string(Path::new(path))),
    );
    denied.push(sbpl_string(&closure.work));
    let mut settings = vec![sbpl_string(&config)];
    settings.extend(
        bench
            .model
            .allowed
            .iter()
            .map(|name| sbpl_string(&closure.home.join(name))),
    );
    let withheld: Vec<String> = bench
        .model
        .withheld
        .iter()
        .map(|name| sbpl_string(&config.join(name)))
        .collect();
    [
        "(version 1)".to_owned(),
        "(allow default)".to_owned(),
        format!("(deny file-read* file-write* {})", subpaths(&denied)),
        format!(
            "(allow file-read-metadata (literal {}))",
            sbpl_string(&closure.home)
        ),
        format!("(allow file-read* file-write* {})", subpaths(&settings)),
        format!("(deny file-read* file-write* {})", subpaths(&withheld)),
        format!(
            "(allow file-read* file-write* (subpath {}))",
            sbpl_string(&closure.scratch)
        ),
    ]
    .join("\n")
}

/// The model command, called closed-book.
pub(super) struct CliAuthor<'run> {
    /// The benchmark.
    pub(super) bench: &'run Bench,
    /// The model command.
    pub(super) command: &'run str,
    /// The model's id.
    pub(super) model: &'run str,
    /// This trial's scratch directory.
    pub(super) scratch: PathBuf,
    /// The run's work root.
    pub(super) work: &'run Path,
    /// The children the run owns.
    pub(super) children: &'run Children,
    /// How long one call may take.
    pub(super) bound: Duration,
}

/// `path` resolved, naming it when it cannot be.
fn resolved(path: &Path) -> Result<PathBuf, BenchError> {
    match fs::canonicalize(path) {
        Ok(found) => Ok(found),
        Err(cause) => fail(
            BenchError::Io {
                path: path.to_path_buf(),
                cause,
            },
            "resolved",
        ),
    }
}

impl CliAuthor<'_> {
    /// The resolved paths the profile of this trial names.
    fn closure(&self) -> Result<Closure, BenchError> {
        let Some(home) = std::env::var_os("HOME") else {
            return fail(
                BenchError::Refused("HOME is not set, so the sandbox cannot name it".to_owned()),
                "closure",
            );
        };
        Ok(Closure {
            home: resolved(Path::new(&home))?,
            work: resolved(self.work)?,
            scratch: resolved(&self.scratch)?,
        })
    }

    /// The environment of one call: the run's own, then the manifest's entries
    /// with `{scratch}` filled in. A directory an entry names under the scratch
    /// directory is made, so a command that keeps its state there finds it.
    fn call_env(&self, scratch: &Path) -> Result<BTreeMap<String, String>, BenchError> {
        let mut env = inherited_env();
        let scratch = scratch.display().to_string();
        for entry in &self.bench.model.env {
            let Some((name, value)) = entry.split_once('=') else {
                return fail(
                    BenchError::Manifest(format!("[model] env entry {entry:?} is not NAME=value")),
                    "call_env",
                );
            };
            let value = value.replace("{scratch}", &scratch);
            if value.starts_with(&scratch)
                && let Err(cause) = fs::create_dir_all(&value)
            {
                return fail(
                    BenchError::Io {
                        path: PathBuf::from(&value),
                        cause,
                    },
                    "call_env",
                );
            }
            env.insert(name.to_owned(), value);
        }
        Ok(env)
    }

    /// The command line of one call, under the sandbox.
    fn argv(&self, profile: String) -> Vec<String> {
        let mut argv = vec![
            self.bench.model.sandbox.clone(),
            "-p".to_owned(),
            profile,
            self.command.to_owned(),
        ];
        argv.extend(
            self.bench
                .model
                .args
                .iter()
                .map(|argument| argument.replace("{model}", self.model)),
        );
        argv
    }
}

/// Refuses a host that cannot make a model trial closed-book.
///
/// A trial whose author can read the hidden oracle measures reading, not
/// authoring, so there is no open-book fallback: no sandbox, no model trial.
pub(super) fn ensure_sandbox(bench: &Bench) -> Result<(), BenchError> {
    let sandbox = &bench.model.sandbox;
    if cfg!(target_os = "macos") && Path::new(sandbox).is_file() {
        return Ok(());
    }
    fail(
        BenchError::Refused(format!(
            "a model trial must be closed-book, and this runner enforces that with {sandbox}, which this host lacks"
        )),
        "ensure_sandbox",
    )
}

impl Author for CliAuthor<'_> {
    fn ask(&self, prompt: &str) -> Result<Reply, BenchError> {
        ensure_sandbox(self.bench)?;
        let closure = self.closure()?;
        let profile = closed_book_profile(self.bench, &closure);
        let env = self.call_env(&closure.scratch)?;
        let input_path = self.scratch.join("stdin.txt");
        if let Err(cause) = fs::write(&input_path, prompt) {
            return fail(
                BenchError::Io {
                    path: input_path,
                    cause,
                },
                "ask",
            );
        }
        let input = match File::open(&input_path) {
            Ok(file) => file,
            Err(cause) => {
                return fail(
                    BenchError::Io {
                        path: input_path,
                        cause,
                    },
                    "ask",
                );
            }
        };
        let done = self.children.capture(&Launch {
            argv: &self.argv(profile),
            root: &self.scratch,
            env: &env,
            stdin: Some(&input),
            bound: self.bound,
        });
        let (stdout, stderr) = if done.code == TIMED_OUT {
            (
                String::new(),
                format!("model call exceeded the {} s timeout", self.bound.as_secs()),
            )
        } else {
            (done.stdout, done.stderr)
        };
        let raw = format!("{stdout}\n{stderr}");
        reply_from(&stdout, &stderr, raw)
    }
}

/// What a finished model command's output comes to.
///
/// A reply when either stream carries one. No reply and an `error` event is a
/// failed call, refused with the provider's words so the ledger releases it
/// rather than charging a reservation for an answer that never came, and the
/// trial is a recorded crash that names the cause. No reply and no error event
/// is an empty reply, which the ledger charges whole because its usage is unknown.
fn reply_from(stdout: &str, stderr: &str, raw: String) -> Result<Reply, BenchError> {
    let read = match read_reply(stdout) {
        Some(reply) if !reply.text.trim().is_empty() => Some(reply),
        Some(_) | None => read_reply(stderr),
    };
    if let Some(reply) = read {
        return Ok(Reply { raw, ..reply });
    }
    let failure = match read_failure(stdout) {
        Some(found) => Some(found),
        None => read_failure(stderr),
    };
    match failure {
        Some(why) => fail(
            BenchError::Refused(format!("the model command reported an error: {why}")),
            "reply_from",
        ),
        None => Ok(Reply {
            text: String::new(),
            tokens_in: 0,
            tokens_out: 0,
            wall_ms: 0,
            raw,
        }),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{
        Closure, Texts, closed_book_profile, extract_rust_block, full_prompt, read_reply,
        repair_prompt, reply_from,
    };
    use crate::BenchError;
    use crate::sim_kit::committed;

    /// A real call of 2026-10-18 (opencode 1.18.34): the Go key had expired and the
    /// provider answered 401 before any step ran, so no text and no usage came back.
    const EXPIRED: &str = "{\"type\":\"error\",\"error\":{\"name\":\"APIError\",\"data\":{\"message\":\"This API key has expired.\",\"statusCode\":401}}}\n";

    /// What a call's output comes to, as text: the refusal's words, or `reply:` and
    /// the reply's text and billed tokens.
    fn outcome(stdout: &str, stderr: &str) -> String {
        match reply_from(stdout, stderr, String::new()) {
            Err(BenchError::Refused(why)) => why,
            Err(other) => other.to_string(),
            Ok(reply) => format!("reply:{}:{}", reply.text, reply.tokens_in),
        }
    }

    #[test]
    fn a_call_the_provider_refused_is_a_failed_call_and_never_an_empty_reply() {
        assert_eq!(
            outcome(EXPIRED, ""),
            "the model command reported an error: HTTP 401: This API key has expired.",
            "the refusal carries the provider's status and words, so the ledger releases the call"
        );
        assert_eq!(
            outcome("", EXPIRED),
            outcome(EXPIRED, ""),
            "an error printed on stderr is read the same way"
        );
        assert_eq!(
            outcome("", ""),
            "reply::0",
            "output with no reply and no error is an empty reply, charged whole by the ledger"
        );
        assert_eq!(
            outcome(&format!("{{\"finalText\":\"done\"}}\n{EXPIRED}"), ""),
            "reply:done:0",
            "a call that answered keeps its answer even if an error event follows"
        );
    }

    /// A profile's lines for a closure of three named directories.
    fn profile_of(
        home: &str,
        work: &str,
        scratch: &str,
    ) -> Result<String, Box<dyn std::error::Error>> {
        Ok(closed_book_profile(
            &committed()?,
            &Closure {
                home: PathBuf::from(home),
                work: PathBuf::from(work),
                scratch: PathBuf::from(scratch),
            },
        ))
    }

    /// The model command's own reply contract is one fenced block; a model that
    /// quotes an earlier attempt and then corrects it means the later block.
    #[test]
    fn the_last_rust_block_is_the_solution() {
        let reply =
            "Here you go:\n```rust\nfn a() {}\n```\nor better:\n```rust\nfn b() {}\n```\nthanks";
        assert_eq!(
            extract_rust_block(reply),
            "fn b() {}\n",
            "the later rust block wins"
        );
        assert_eq!(
            extract_rust_block("```rust\nA\n```\n```toml\nB\n```"),
            "A\n",
            "a rust block beats a later block of another language"
        );
        assert_eq!(
            extract_rust_block("```\nplain\n```"),
            "plain\n",
            "with no rust block the last block of any language is used"
        );
    }

    /// CommonMark 0.31 §4.5: an info string follows the opening fence, and a
    /// fence may be longer than three backticks; Windows tools end lines in CRLF.
    #[test]
    fn a_fence_with_extra_backticks_or_windows_line_ends_still_opens() {
        assert_eq!(
            extract_rust_block("````rust\ncode\n```"),
            "code\n",
            "a four-backtick opener opens at its last three"
        );
        assert_eq!(
            extract_rust_block("```rust\r\ncode\r\n```"),
            "code\n",
            "carriage returns are trimmed from the block and its opener"
        );
        assert_eq!(
            extract_rust_block("```rust   \nspaced\n```"),
            "spaced\n",
            "blanks after the language word are allowed"
        );
    }

    #[test]
    fn a_reply_with_no_closed_block_has_no_code() {
        assert_eq!(
            extract_rust_block("no fences at all"),
            "",
            "prose has no code"
        );
        assert_eq!(
            extract_rust_block("```rust\nnever closed"),
            "",
            "an unclosed fence is not a block"
        );
        assert_eq!(
            extract_rust_block("```rustc-style words\nx\n```"),
            "",
            "an info string with words after the language is not an opener"
        );
    }

    #[test]
    fn the_role_comes_first_then_the_task_then_the_sheet_under_the_framing() {
        let texts = Texts {
            preamble: "ROLE TEXT".to_owned(),
            task: "TASK TEXT".to_owned(),
            sheet: "SHEET TEXT".to_owned(),
        };
        let prompt = full_prompt("FRAMING", &texts);
        let at = |needle: &str| prompt.find(needle);
        assert!(
            prompt.starts_with("FRAMING\n\n"),
            "the framing opens the skeleton"
        );
        assert!(
            at("=== ROLE ===") < at("=== TASK ===") && at("=== TASK ===") < at("=== API SHEET ==="),
            "role, then task, then sheet"
        );
        let bare = full_prompt(
            "FRAMING",
            &Texts {
                preamble: String::new(),
                task: "T".to_owned(),
                sheet: "S".to_owned(),
            },
        );
        assert!(!bare.contains("=== ROLE ==="), "no profile, no role block");
    }

    #[test]
    fn a_repair_prompt_carries_the_code_and_the_first_eighty_lines_of_errors() {
        let errors: String = (1..=100)
            .map(|line| format!("error line {line}\n"))
            .collect();
        let prompt = repair_prompt("ORIGINAL", "fn broken() {}\n", &errors);
        assert!(
            prompt.starts_with("ORIGINAL"),
            "the original prompt comes first"
        );
        assert!(
            prompt.contains("```rust\nfn broken() {}\n\n```"),
            "the code under repair is shown"
        );
        assert!(
            prompt.contains("error line 80\n"),
            "the eightieth line is kept"
        );
        assert!(
            !prompt.contains("error line 81\n"),
            "the eighty-first line is cut"
        );
    }

    #[test]
    fn a_reply_without_code_is_told_so_rather_than_shown_an_empty_crate() {
        let prompt = repair_prompt("ORIGINAL", "", "ignored errors");
        assert!(
            prompt.contains("contained no fenced ```rust block"),
            "the repair says what was missing"
        );
        assert!(
            !prompt.contains("ignored errors"),
            "there is nothing to quote the compiler on"
        );
    }

    /// The model command's final NDJSON result line, as `--output-format json`
    /// prints it after its progress lines.
    #[test]
    fn the_result_line_gives_text_tokens_and_the_models_own_wall_time() {
        let raw = "{\"type\":\"progress\",\"n\":1}\nnot json at all\n{\"type\":\"result\",\"finalText\":\"```rust\\nfn a() {}\\n```\",\"usage\":{\"inputTokens\":120,\"outputTokens\":30},\"durationMs\":4567}\n";
        let reply = read_reply(raw);
        assert_eq!(
            reply
                .as_ref()
                .map(|found| (found.tokens_in, found.tokens_out, found.wall_ms)),
            Some((120, 30, 4567)),
            "tokens and duration come from the result line"
        );
        assert!(
            reply.is_some_and(|found| found.text.contains("fn a() {}")),
            "the final text is the reply"
        );
    }

    #[test]
    fn the_last_result_line_wins_and_a_missing_one_is_no_reply() {
        let two = "{\"finalText\":\"first\"}\n{\"finalText\":\"second\"}\n";
        assert_eq!(
            read_reply(two).map(|found| found.text),
            Some("second".to_owned()),
            "the final result is the last one"
        );
        assert!(
            read_reply("{\"type\":\"progress\"}\n").is_none(),
            "progress is not a result"
        );
        assert_eq!(
            read_reply("{\"type\":\"result\"}\n").map(|found| (found.text, found.tokens_in)),
            Some((String::new(), 0)),
            "a result without text or usage is an empty reply, counted as zero tokens"
        );
    }

    /// sandbox-exec(1) / Apple SBPL: rules are evaluated in order and the last
    /// match wins, so the order is the policy: deny, let the settings back in,
    /// deny what the settings hold that must stay hidden, allow the scratch.
    #[test]
    fn the_closed_book_profile_orders_its_rules_so_the_withheld_stay_withheld()
    -> Result<(), Box<dyn std::error::Error>> {
        let bench = committed()?;
        let profile = profile_of("home-dir", "work-root", "scratch-dir")?;
        let lines: Vec<&str> = profile.lines().collect();
        assert_eq!(
            lines.len(),
            7,
            "version, default, deny, metadata, settings, withheld, scratch"
        );
        let at = |index: usize| match lines.get(index) {
            Some(line) => *line,
            None => "",
        };
        assert_eq!(at(0), "(version 1)", "the profile is version 1");
        assert_eq!(
            at(1),
            "(allow default)",
            "everything not named stays allowed"
        );
        assert!(
            at(2).starts_with("(deny file-read* file-write* (subpath \"home-dir\")")
                && at(2).ends_with("(subpath \"work-root\"))"),
            "home first, then the named directories, then the work root: {}",
            at(2)
        );
        for denied in &bench.model.denied {
            assert!(
                at(2).contains(&format!("(subpath \"{denied}\")")),
                "the manifest's denied directory {denied} is denied"
            );
        }
        assert_eq!(
            at(3),
            "(allow file-read-metadata (literal \"home-dir\"))",
            "the home directory itself can be stat'ed, nothing beneath it"
        );
        let settings = std::iter::once(bench.model.config_dir.as_str())
            .chain(bench.model.allowed.iter().map(String::as_str))
            .map(|name| format!("(subpath \"home-dir/{name}\")"))
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(
            at(4),
            format!("(allow file-read* file-write* {settings})"),
            "the command's own settings, and the home directories the manifest allows, are let back in"
        );
        assert!(
            at(5).starts_with("(deny file-read* file-write* "),
            "then the withheld are denied again"
        );
        for name in &bench.model.withheld {
            assert!(
                at(5).contains(&format!(
                    "(subpath \"home-dir/{}/{name}\")",
                    bench.model.config_dir
                )),
                "{name} stays withheld"
            );
        }
        assert_eq!(
            at(6),
            "(allow file-read* file-write* (subpath \"scratch-dir\"))",
            "the trial's scratch comes last, so it wins over every denial above"
        );
        Ok(())
    }

    #[test]
    fn a_path_with_a_quote_or_backslash_cannot_close_the_profile_string()
    -> Result<(), Box<dyn std::error::Error>> {
        assert!(
            profile_of("h\"ome", "w", "s")?.contains("(subpath \"h\\\"ome\")"),
            "the quote is escaped inside the SBPL string literal"
        );
        assert!(
            profile_of("a\\b", "w", "s")?.contains("(subpath \"a\\\\b\")"),
            "the backslash is doubled so it cannot escape the closing quote"
        );
        Ok(())
    }
}
