//! A fake `gh` on PATH, and the recordings the tests assert on.
//!
//! The adapter resolves its program through `PATH`, so a test binds it to a
//! script it controls by putting a directory first on `PATH` and naming `gh`
//! inside it. Nothing here mocks a Rust function: the code under test forks,
//! captures stdout and stderr, applies a deadline and reaps a process group
//! exactly as it would for the real client.
//!
//! The script records one line of argv per invocation, appending to a log file
//! whose path arrives in an environment variable. A JSON file beside it carries
//! the behaviour: which head a read reports, whether a create lands, and
//! whether the create's response is dropped after the effect was applied.
//!
//! One copy, included by path from every target that needs a fake `gh`, so two
//! targets cannot assert different things about the same fake.

// A single test target owns the fake, so this module is only ever included once
// and the unused-item tolerance is not needed.

#![allow(dead_code, reason = "one including target may use a different subset")]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Monotone per-process sequence, so two fixtures in one binary never collide.
static DIR_SEQ: AtomicU64 = AtomicU64::new(0);

/// A fake `gh` installed on `PATH` for one scenario.
///
/// The directory is created under the system temp directory, removed on drop,
/// and never depends on a path the repository owns — the fixture is portable
/// and a test leaves nothing behind.
pub struct FakeGh {
    /// The directory to place first on `PATH`.
    dir: PathBuf,
    /// The file the fake appends one argv line to per invocation.
    log: PathBuf,
}

impl FakeGh {
    /// Install a fake `gh` and return its handle.
    ///
    /// The behaviour file starts as a record of every read reporting `head_sha`
    /// and no create landing, so a scenario that changes nothing gets a
    /// coherent "GitHub has this pull request and no review" world rather than
    /// an empty one the script has to interpret.
    pub fn install(name: &str, head_sha: &str) -> std::io::Result<Self> {
        let tag = std::process::id();
        let seq = DIR_SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("lgwks-fake-gh-{tag}-{seq}-{name}"));
        std::fs::create_dir_all(&dir)?;
        let log = dir.join("argv.log");
        std::fs::write(&log, b"")?;

        let behaviour = dir.join("behaviour.json");
        std::fs::write(
            &behaviour,
            format!(
                "{{\"head_sha\":\"{head_sha}\",\"base_sha\":\"{}\",\"create\":\"accept\",\
                 \"reviews\":[],\"next_review_id\":9001,\"created_body\":\"\",\
                 \"created_state\":\"COMMENT\",\"fail_reads\":0,\"hang_seconds\":0,\
                 \"flood_bytes\":0}}\n",
                "b".repeat(40)
            ),
        )?;

        let script = dir.join("gh");
        std::fs::write(&script, FAKE_GH_SOURCE)?;
        make_executable(&script)?;
        Ok(Self { dir, log })
    }

    /// Replace the behaviour file with the scenario's own receiver state.
    ///
    /// One writer, because every scenario writes the same five fields and a
    /// per-test JSON literal is a copy that drifts: a key the fake stopped
    /// reading would still be present in one test's literal and absent in
    /// another's, and the difference would look like a behaviour difference.
    /// An unspecified field takes the value the fake already defaults to.
    pub fn configure(&self, scenario: Scenario) -> std::io::Result<()> {
        std::fs::write(self.behaviour(), scenario.to_behaviour())
    }

    /// The behaviour file the fake reads on each invocation.
    pub fn behaviour(&self) -> PathBuf {
        self.dir.join("behaviour.json")
    }

    /// The directory to place first on `PATH`.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The program name the adapter resolves through `PATH`.
    ///
    /// A bare `gh`, so the test exercises real `PATH` resolution rather than
    /// pinning an absolute path — which is exactly what a deployment does.
    pub const fn program(&self) -> &'static str {
        "gh"
    }

    /// Every invocation's argv, in order, one per line with tab separators.
    pub fn calls(&self) -> std::io::Result<Vec<Vec<String>>> {
        let text = std::fs::read_to_string(&self.log)?;
        Ok(text
            .lines()
            .filter(|line| !line.is_empty())
            .map(|line| line.split('\t').map(str::to_owned).collect())
            .collect())
    }

    /// How many invocations used `--method POST` — that is, how many reviews
    /// were *created*, which is the number a duplicate-post defect doubles.
    pub fn creates(&self) -> std::io::Result<usize> {
        Ok(self
            .calls()?
            .iter()
            .filter(|argv| {
                argv.windows(2)
                    .any(|pair| pair[0] == "--method" && pair[1] == "POST")
            })
            .count())
    }

    /// How many invocations used `--method GET` against `suffix`.
    pub fn reads_of(&self, suffix: &str) -> std::io::Result<usize> {
        Ok(self
            .calls()?
            .iter()
            .filter(|argv| argv.iter().any(|arg| arg.ends_with(suffix)))
            .count())
    }

    /// Every payload the fake received, in order.
    ///
    /// Read from the receiver's own record rather than from the staged file,
    /// because the adapter removes that file as soon as the child exits — a
    /// test that read it afterwards would be asserting on nothing.
    pub fn received(&self) -> std::io::Result<Vec<String>> {
        let path = self.dir.join("received.jsonl");
        if !path.exists() {
            return Ok(Vec::new());
        }
        Ok(std::fs::read_to_string(path)?
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(str::to_owned)
            .collect())
    }

    /// The staged payload of the nth create, or `None` when there was none.
    pub fn payload_of(&self, index: usize) -> std::io::Result<Option<String>> {
        let calls = self.calls()?;
        let creates = calls
            .iter()
            .filter(|argv| {
                argv.windows(2)
                    .any(|pair| pair[0] == "--method" && pair[1] == "POST")
            })
            .count();
        if index >= creates {
            return Ok(None);
        }
        let received = self.received()?;
        Ok(received.get(index).cloned())
    }
}

impl Drop for FakeGh {
    /// Remove the fixture, best effort.
    fn drop(&mut self) {
        let _ignored = std::fs::remove_dir_all(&self.dir);
    }
}

/// Make `path` executable by whoever owns it.
#[cfg(unix)]
fn make_executable(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    let mut permissions = std::fs::metadata(path)?.permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(path, permissions)
}

/// Nothing to do off Unix: the fake is a `#!/bin/sh` script.
#[cfg(not(unix))]
fn make_executable(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

/// The fake client itself.
///
/// POSIX `sh`, no `jq`, no `python`: the estate's portability rules apply to a
/// fixture too, and a fake that needs a JSON processor to fake a JSON client is
/// a fixture with its own dependency. The behaviour file is read with `sed`, and
/// the reviews list is emitted verbatim from the file so a scenario controls
/// exactly what the read-back returns.
///
/// The argv record is written **before** the behaviour is applied, so a create
/// that dies mid-way is still recorded. That is the point: the question the
/// tests ask is how many times the code under test *issued* a create, which is
/// what this log counts.
const FAKE_GH_SOURCE: &str = r#"#!/bin/sh
# A fake `gh api` for the adapter's tests. Records argv, then answers from a
# behaviour file. Never touches the network.
set -u

dir=$(dirname "$0")
log="$dir/argv.log"
behaviour="$dir/behaviour.json"

# Record argv first: a call that fails afterwards still happened, and counting
# the calls that *started* is the measurement the duplicate-post tests need.
# The whole line is assembled in memory and written with one append, so two
# concurrent runs of the fake cannot interleave halves of a line.
line=""
for arg in "$@"; do
  line="$line$arg$(printf '\t')"
done
printf '%s\n' "$line" >> "$log"

# One scalar field out of the behaviour file. A quoted string keeps its
# spaces, because the shell splits on them otherwise.
field() {
  quoted=$(sed -n "s/.*\"$1\":\"\([^\"]*\)\".*/\1/p" "$behaviour" | head -n 1)
  if [ -n "$quoted" ]; then
    printf '%s' "$quoted"
    return
  fi
  sed -n "s/.*\"$1\":[^0-9a-zA-Z]*\([^\",}]*\).*/\1/p" "$behaviour" | head -n 1
}

# The staged payload, when this call has one.
payload=""
previous=""
for arg in "$@"; do
  if [ "$previous" = "--input" ] && [ -f "$arg" ]; then
    payload=$(cat "$arg")
    # The adapter removes the staged file as soon as the child exits, so the
    # payload is copied into the receiver's own store here: what the receiver
    # actually received, which is what a test asserts against.
    printf '%s\n' "$payload" >> "$dir/received.jsonl"
  fi
  previous="$arg"
done

method="GET"
previous=""
for arg in "$@"; do
  if [ "$previous" = "--method" ]; then method="$arg"; fi
  previous="$arg"
done

# Optional flooding, for the bounded-capture test: emit `flood_bytes` of
# padding on stdout before anything else, so the adapter's ceiling is what
# decides what it retains.
flood=$(field flood_bytes)
if [ -n "${flood:-}" ] && [ "$flood" -gt 0 ] 2>/dev/null; then
  i=0
  while [ "$i" -lt "$flood" ]; do
    printf 'PADDINGPADDINGPADDINGPADDINGPADDINGPADDINGPADDINGPADDING\n'
    i=$((i + 1))
  done
fi

# An optional hang, for the deadline test.
hang=$(field hang_seconds)
if [ -n "${hang:-}" ] && [ "$hang" -gt 0 ] 2>/dev/null; then
  # A grandchild, so the deadline has a process *group* to reap rather than
  # one child it could trivially kill.
  ( sleep "$hang" ) &
  sleep "$hang"
  exit 0
fi

if [ "$method" = "POST" ]; then
  create=$(field create)
  case "$create" in
    accept)
      # The receiver applies the review: it records what was asked for in its
      # own store, so a later read-back returns it. That is the whole point of
      # a receiver-backed fixture — the read-back is not a canned answer, it is
      # the state the write actually produced, which is what makes the
      # lost-response test discriminating rather than circular.
      id=$(field next_review_id)
      state=$(field created_state)
      event=$(printf '%s' "$payload" | sed -n 's/.*"event":"\([A-Z_]*\)".*/\1/p')
      if [ -n "$event" ]; then state="$event"; fi
      printf '%s\n' "$payload" > "$dir/applied-$id.json"
      printf '{"id":%s,"commit_id":"%s","state":"%s","body":"%s"}\n' \
        "$id" "$(field head_sha)" "$state" "$(field created_body)"
      exit 0
      ;;
    accept_then_drop)
      # The effect lands and the *response* is lost: the review is recorded in
      # the receiver, then the connection dies before anything is printed. This
      # is the case the whole reconciliation path exists for.
      id=$(field next_review_id)
      printf '%s\n' "$payload" > "$dir/applied-$id.json"
      printf 'connection reset by peer\n' >&2
      exit 7
      ;;
    *)
      printf 'create failed: %s\n' "$create" >&2
      exit 1
      ;;
  esac
fi

# A read. The review list is identified by scanning argv for a `.../reviews`
# path, not by position: `gh api --method GET .../reviews --paginate` puts a
# flag after the path, so a suffix match would read the pull request instead.
is_reviews=0
previous=""
for arg in "$@"; do
  case "$arg" in
    */reviews) is_reviews=1 ;;
  esac
  previous="$arg"
done

if [ "$is_reviews" -eq 1 ]; then
    fail=$(field fail_reads)
    if [ -n "${fail:-}" ] && [ "$fail" -gt 0 ] 2>/dev/null; then
      printf 'read refused by scenario\n' >&2
      exit 1
    fi
    # The list is built from what the receiver actually applied, so a scenario
    # that posted one review reads back exactly that review. `sh` is not asked
    # to parse JSON: each applied payload is copied verbatim, and only its id,
    # commit, state and body are re-emitted from the fields the receiver also
    # stored beside it.
    printf '['
    first=1
    for applied in "$dir"/applied-*.json; do
      [ -e "$applied" ] || continue
      id=$(basename "$applied" .json)
      id=${id#applied-}
      body=$(sed -n 's/.*"body":"\([^"]*\)".*/\1/p' "$applied" | head -n 1)
      event=$(sed -n 's/.*"event":"\([A-Z_]*\)".*/\1/p' "$applied" | head -n 1)
      commit=$(sed -n 's/.*"commit_id":"\([^"]*\)".*/\1/p' "$applied" | head -n 1)
      if [ "$first" -eq 0 ]; then printf ','; fi
      first=0
      printf '{"id":%s,"commit_id":"%s","state":"%s","body":"%s"}' \
        "$id" "$commit" "$event" "$body"
    done
  printf ']\n'
else
  # The pull-request read. `head_after_first` models a head that moves between
  # two reads: the first read reports `head_sha`, and every read after it
  # reports `head_after_first` — so a freshness check comparing the two reads
  # sees exactly the change it is meant to catch. The counter is bumped with a
  # single append per call, so two concurrent runs cannot interleave it.
  printf 'x' >> "$dir/reads"
  read_count=$(wc -c < "$dir/reads" | tr -d ' ')
  head=$(field head_sha)
  moved=$(field head_after_first)
  if [ -n "${moved:-}" ] && [ "$read_count" -gt 1 ]; then head="$moved"; fi
  printf '{"number":7,"head_sha":"%s","base_sha":"%s"}\n' \
    "$head" "$(field base_sha)"
fi
exit 0
"#;

/// What a scenario tells the fake receiver to do.
///
/// A builder rather than a JSON literal per test: the fields are named once,
/// the defaults are stated once, and a scenario reads as the difference from a
/// working world rather than as a wall of escaped braces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scenario {
    /// The head the first pull-request read reports.
    head: String,
    /// The head every read after the first reports, modelling a push landing
    /// between two reads. `None` leaves the head fixed.
    pub head_after_first: Option<String>,
    /// What a create does: `accept`, `accept_then_drop`, or anything else,
    /// which is treated as a refusal.
    pub create: &'static str,
    /// The body a created review carries, which is what the read-back returns
    /// and therefore what the verification compares against.
    created_body: String,
    /// The state a created review reports.
    pub created_state: &'static str,
    /// How many pull-request reads are refused before one succeeds.
    pub fail_reads: u32,
    /// How long a call hangs, for the deadline probe. `0` hangs not at all.
    pub hang_seconds: u32,
    /// How many padding lines a call writes to stdout before its answer, for
    /// the bounded-capture probe.
    pub flood_bytes: u32,
}

impl Scenario {
    /// A world where the head is fixed, creates are accepted and no read fails.
    #[must_use]
    pub fn new(head: &str) -> Self {
        Self {
            head: head.to_owned(),
            head_after_first: None,
            create: "accept",
            created_body: String::new(),
            created_state: "COMMENT",
            fail_reads: 0,
            hang_seconds: 0,
            flood_bytes: 0,
        }
    }

    /// The head this scenario's first read reports.
    #[must_use]
    pub fn head(&self) -> &str {
        &self.head
    }

    /// The body a created review carries, read back by verification.
    #[must_use]
    pub fn body(&self) -> &str {
        &self.created_body
    }

    /// The head this scenario's later reads report.
    #[must_use]
    pub fn head_after(&self) -> Option<&str> {
        self.head_after_first.as_deref()
    }

    /// A create is accepted and its response then dropped.
    #[must_use]
    pub fn accept_then_drop(mut self) -> Self {
        self.create = "accept_then_drop";
        self
    }

    /// A create is refused.
    #[must_use]
    pub fn refuse_creates(mut self) -> Self {
        self.create = "refuse";
        self
    }

    /// Every read before the `n`th is refused.
    #[must_use]
    pub fn fail_reads(mut self, count: u32) -> Self {
        self.fail_reads = count;
        self
    }

    /// A created review carries `body`.
    #[must_use]
    pub fn created_body(mut self, body: &str) -> Self {
        body.clone_into(&mut self.created_body);
        self
    }

    /// The head moves to `head` after the first read.
    #[must_use]
    pub fn head_moves_to(mut self, head: &str) -> Self {
        self.head_after_first = Some(head.to_owned());
        self
    }

    /// A call hangs for `seconds` before it answers.
    #[must_use]
    pub fn hangs_for(mut self, seconds: u32) -> Self {
        self.hang_seconds = seconds;
        self
    }

    /// A call writes `bytes` padding lines before its answer.
    #[must_use]
    pub fn floods(mut self, bytes: u32) -> Self {
        self.flood_bytes = bytes;
        self
    }

    /// The behaviour file this scenario is, as the fake reads it.
    ///
    /// Written here rather than assembled by each test so the keys the fake
    /// looks up are the keys that exist, which is the failure mode a literal
    /// per test invites.
    #[must_use]
    pub fn to_behaviour(&self) -> String {
        let base = "b".repeat(40);
        let after = self
            .head_after_first
            .as_ref()
            .map_or_else(String::new, |head| {
                format!("\"head_after_first\":\"{head}\",")
            });
        format!(
            "{{{after}\"head_sha\":\"{head}\",\"base_sha\":\"{base}\",\
             \"create\":\"{create}\",\"next_review_id\":9001,\
             \"created_body\":\"{body}\",\"created_state\":\"{state}\",\
             \"fail_reads\":{fail_reads},\"hang_seconds\":{hang},\"flood_bytes\":{flood}}}\n",
            head = self.head,
            create = self.create,
            body = self.created_body,
            state = self.created_state,
            fail_reads = self.fail_reads,
            hang = self.hang_seconds,
            flood = self.flood_bytes,
        )
    }
}
