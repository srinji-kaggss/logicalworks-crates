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
        // Random rather than a process id: the OS reuses process ids, and two
        // fixtures sharing a directory would read each other's argv log — which
        // is the one thing these tests measure.
        // An entropy source that cannot answer refuses the fixture rather than
        // naming it zero: a zero every failed draw shares is the collision the
        // random tag exists to rule out.
        let tag = lgwks_std::random::bytes::<8>()
            .map(u64::from_le_bytes)
            .map_err(std::io::Error::other)?;
        let seq = DIR_SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("lgwks-fake-gh-{tag:016x}-{seq}-{name}"));
        std::fs::create_dir_all(&dir)?;
        let log = dir.join("argv.log");
        std::fs::write(&log, b"")?;

        let behaviour = dir.join("behaviour.json");
        std::fs::write(&behaviour, Scenario::new(head_sha).to_behaviour())?;

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

    /// The `PATH` this fake is found through: its directory in front of the
    /// ambient one.
    ///
    /// Handed to a binding as a per-child variable rather than set on this
    /// process, which keeps tests independent and needs no `unsafe`.
    pub fn search_path(&self) -> Result<std::ffi::OsString, std::env::JoinPathsError> {
        let ambient = match std::env::var_os("PATH") {
            Some(path) => path,
            None => std::ffi::OsString::new(),
        };
        let mut entries = vec![self.dir.clone()];
        entries.extend(std::env::split_paths(&ambient));
        std::env::join_paths(&entries)
    }

    /// The program name the adapter resolves through `PATH`.
    ///
    /// A bare `gh`, so the test exercises real `PATH` resolution rather than
    /// pinning an absolute path — which is exactly what a deployment does.
    pub const fn program(&self) -> &'static str {
        "gh"
    }

    /// A [`Gh`](lgwks_bot::domain::gh::Gh) binding pointed at this fake, with
    /// the named capture ceiling.
    ///
    /// One definition rather than a per-test builder, because the two lines of
    /// binding are what make a test "run through the adapter", and a copy is a
    /// place for one test to quietly stop doing it.
    pub fn binding(
        &self,
        repository: &str,
        capture: usize,
    ) -> Result<lgwks_bot::domain::gh::Gh, Box<dyn std::error::Error>> {
        Ok(
            lgwks_bot::domain::gh::Gh::new(lgwks_bot::domain::gh::Repository::new(repository)?)
                .program(self.program())
                .capture_limit(
                    std::num::NonZeroUsize::new(capture).ok_or("a non-zero capture ceiling")?,
                )
                .deadline(Some(std::time::Duration::from_secs(20)))
                .env("PATH", self.search_path()?),
        )
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

# Every fork below is paid once per call, and a saturation family makes tens of
# thousands of calls, so the bookkeeping avoids command substitution wherever a
# parameter expansion answers the same question. `$0` is the path `execvp`
# resolved, so stripping its last component is what `dirname` printed.
case "$0" in
  */*) dir=${0%/*} ;;
  *) dir=. ;;
esac
log="$dir/argv.log"
behaviour="$dir/behaviour.json"

# Record argv first: a call that fails afterwards still happened, and counting
# the calls that *started* is the measurement the duplicate-post tests need.
# The whole line is assembled in memory and written with one append, so two
# concurrent runs of the fake cannot interleave halves of a line.
# A literal tab, kept out of a command substitution: a saturation family makes
# tens of thousands of calls and every `$(...)` is a fork of its own.
tab='	'
line=""
for arg in "$@"; do
  line="$line$arg$tab"
done
printf '%s\n' "$line" >> "$log"

# The behaviour file is one line of JSON the fixture itself writes, so a field
# is read once per invocation and cut with parameter expansion rather than by
# forking `sed` per lookup. A saturation family runs five calls per review
# (snapshot, diff, freshness, publish, verify), and the fixture's own
# bookkeeping must not dominate the measurement, so the read is one builtin and
# each lookup is one expansion.
#
# Read once rather than per lookup: a key present under neither the quoted nor
# the numeric shape yields the empty string, which is exactly what the callers
# treat as "unspecified". The value charset is the fixture's own — alphanumeric
# plus `:._-` — so a value ends at the first character outside it.
BEHAVIOUR=""
IFS= read -r BEHAVIOUR < "$behaviour"

field() {
  val=""
  q="\"$1\":\""
  rest=${BEHAVIOUR#*"$q"}
  if [ "$rest" = "$BEHAVIOUR" ]; then
    u="\"$1\":"
    rest=${BEHAVIOUR#*"$u"}
    if [ "$rest" = "$BEHAVIOUR" ]; then
      return 0
    fi
  fi
  val=${rest%%[!a-zA-Z0-9:._-]*}
}

# The staged payload, when this call has one.
payload=""
previous=""
for arg in "$@"; do
  if [ "$previous" = "--input" ] && [ -f "$arg" ]; then
    # The staged payload is one line of JSON the adapter wrote, so it is read
    # with the builtin rather than forked through `cat`.
    IFS= read -r payload < "$arg"
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
field flood_bytes; flood=$val
if [ -n "${flood:-}" ] && [ "$flood" -gt 0 ] 2>/dev/null; then
  i=0
  while [ "$i" -lt "$flood" ]; do
    printf 'PADDINGPADDINGPADDINGPADDINGPADDINGPADDINGPADDINGPADDING\n'
    i=$((i + 1))
  done
fi

# An optional hang, for the deadline test.
field hang_seconds; hang=$val
if [ -n "${hang:-}" ] && [ "$hang" -gt 0 ] 2>/dev/null; then
  # A grandchild, so the deadline has a process *group* to reap rather than
  # one child it could trivially kill.
  ( sleep "$hang" ) &
  sleep "$hang"
  exit 0
fi

if [ "$method" = "POST" ]; then
  field create; create=$val
  case "$create" in
    accept|accept_then_drop|pending|partial)
      # A real receiver assigns a fresh id per accepted create. The append is
      # one atomic write, so the count of accepted creates is exact; the count
      # is read back with the builtin (`read` takes the append log whole and
      # `${#..}` is its byte count) rather than by forking `wc`. The read
      # follows the append, so two creates racing here can read the same count
      # and share an id — the same window this counter always had — and that is
      # safe: a read-back reconciles on subject, body and state, and every
      # id-asserting family runs its creates sequentially.
      printf 'x' >> "$dir/creates"
      seen_raw=""
      IFS= read -r seen_raw < "$dir/creates"
      seen=${#seen_raw}
      field next_review_id
      id=$(( val + seen - 1 ))
      # GitHub reports the state a review is *in*, not the event that created
      # it: `COMMENT` reads back as `COMMENTED`, and so on. A fake that echoed
      # the event would let a verifier pass here that never matches GitHub.
      field created_state; state=$val
      event=${payload##*\"event\":\"}
      event=${event%%\"*}
      case "$event" in
        COMMENT) state=COMMENTED ;;
        APPROVE) state=APPROVED ;;
        REQUEST_CHANGES) state=CHANGES_REQUESTED ;;
      esac
      # A pending draft is never submitted, whatever event the request named:
      # it is the state GitHub holds a create in when no event is submitted, and
      # the read-back must report it as a draft rather than as a publication.
      if [ "$create" = "pending" ]; then state=PENDING; fi
      # The record is rendered from the *payload the adapter sent*, not from the
      # scenario's defaults. That is what makes the read-back a real
      # observation: a receiver that answered from its own configuration would
      # verify a body the adapter never published, and the lost-response test
      # would pass without the write having produced anything.
      #
      # An inline comment also carries a `body`, and the match is greedy, so the
      # comments array is removed before the top-level body is read: otherwise a
      # payload with comments would record the last comment's text as the review
      # body.
      top=${payload%%,\"comments\"*}
      body=${top##*\"body\":\"}
      body=${body%%\"*}
      commit=${payload##*\"commit_id\":\"}
      commit=${commit%%\"*}
      # How many inline comments this review reports: none for everything but a
      # partial landing, which reports the configured applied count — fewer than
      # the payload intended, which is what makes it a *partial* submission.
      comments=0
      if [ "$create" = "partial" ]; then field applied_comments; comments=$val; fi
      printf '%s\n' "$payload" > "$dir/applied-$id.json"
      # The separator goes into `reviews.jsonl` as **one** `>>` append, always, in
      # front of the record. `>>` opens `O_APPEND`, and two writes by
      # descriptors on a regular file cannot interleave, so one append is atomic
      # against every other append however many creates race.
      #
      # Two earlier spellings were not safe, and both produced a read-back a
      # concurrent receiver could not answer: writing the separator separately,
      # which the kernel interleaved into `[,,{...}]`, and probing for the file
      # to decide whether a separator was owed, which two creates could both
      # read as absent into `{...}{...}`. There is no first record to make bare
      # here, so there is nothing to probe.
      #
      # The record ends with a newline, and that newline is its commit mark. An
      # append is atomic against other *appends*, not against a *reader*: on
      # tmpfs (CI's `TMPDIR=/dev/shm`) the copy goes a page at a time and the
      # file grows as it goes, so a concurrent read-back can see the front half
      # of another run's record. CI saw exactly that, a read-back cut mid-string
      # at byte 8,193 (two pages). A JSON-encoded record never holds a raw
      # newline, so a line without one is a record still being written, and the
      # reader below leaves it out.
      printf ',{"id":%s,"commit_id":"%s","state":"%s","body":"%s","comment_count":%s}\n' \
        "$id" "$commit" "$state" "$body" "${comments:-0}" >> "$dir/reviews.jsonl"
      if [ "$create" = "accept" ]; then
        printf '{"id":%s,"commit_id":"%s","state":"%s","body":"%s","comment_count":%s}\n' \
          "$id" "$commit" "$state" "$body" "${comments:-0}"
        exit 0
      fi
      # The effect lands and the *response* is lost: the review is recorded in
      # the receiver, then the connection dies before anything is printed. This
      # is the case the whole reconciliation path exists for.
      printf 'connection reset by peer\n' >&2
      exit 7
      ;;
    *)
      printf 'create failed: %s\n' "$create" >&2
      exit 1
      ;;
  esac
fi

# A read. The review list and the changed-file inventory are each identified by
# scanning argv for their path, not by position: `gh api --method GET
# .../reviews --paginate` puts a flag after the path, so a suffix match over the
# whole vector would read the pull request instead.
is_reviews=0
is_files=0
for arg in "$@"; do
  case "$arg" in
    */reviews) is_reviews=1 ;;
    */files) is_files=1 ;;
  esac
done

if [ "$is_reviews" -eq 1 ]; then
    field fail_reads; fail=$val
    if [ -n "${fail:-}" ] && [ "$fail" -gt 0 ] 2>/dev/null; then
      printf 'read refused by scenario\n' >&2
      exit 1
    fi
    # A permission refusal, for the lost-read-permission probe: the shape `gh`
    # prints when a credential cannot reach the resource, which the adapter
    # reports as its own typed permission failure.
    field deny_reads; deny=$val
    if [ "$deny" = "1" ]; then
      printf 'gh: Bad credentials (HTTP 403)\n' >&2
      exit 1
    fi
    # A malformed answer, for the decode-refusal probes. `garbage` is not JSON;
    # `truncated` is a JSON document that lost its closing bracket, which is the
    # shape a stream cut mid-write takes when everything before the cut was
    # valid. Both must be refused rather than decoded into a partial list.
    field reviews_shape; shape=$val
    case "$shape" in
      garbage)
        printf 'gh: this is not what you asked for\n'
        exit 0
        ;;
    esac
    # The list is the receiver's own store, read back as a document. `sh` is not
    # asked to parse JSON on the way out: each accepted create appended its
    # rendered record to `reviews.jsonl`, so a scenario that posted one review
    # reads back exactly that review and a receiver with many reviews does not
    # pay a `sed` per stored review on every read.
    #
    # The store is `{...},{...}` — a record per create with a *leading*
    # separator, which is what one atomic append can carry. Two earlier
    # spellings were not safe and both produced a read-back a concurrent
    # receiver could not answer: a separator written by its own append, which
    # the kernel interleaved into `[,,{...}]`, and a separator decided by an
    # existence probe, which two creates could both read as absent.
    #
    # So the store's leading separator is dropped on the way out. Each record is
    # one newline-terminated line, and the builtin `read` loop keeps only the
    # lines it saw end: `read` reports a final line with no newline as a
    # failure, so a record another run is still appending — visible half-copied
    # on tmpfs — ends the loop instead of reaching the answer. `${store#?}`
    # then drops the first separator, one byte, without forking a reader on
    # every read-back. An absent or empty store yields an empty string, so `[`
    # is still followed by `]` and a pull request with no reviews reads back as
    # `[]`.
    printf '['
    if [ -s "$dir/reviews.jsonl" ]; then
      store=""
      while IFS= read -r record; do
        store="$store$record"
      done < "$dir/reviews.jsonl"
      printf '%s' "${store#?}"
    fi
    # Filler reviews, for the review-ceiling probe. They are real records on
    # another commit: what matters is that the adapter would have had to read
    # them to call the list complete, so returning only the prefix would be a
    # lie about the pull request's review history.
    field filler_reviews; filler=$val
    i=0
    while [ -n "${filler:-}" ] && [ "$i" -lt "$filler" ] 2>/dev/null; do
      if [ "$i" -eq 0 ] && [ ! -s "$dir/reviews.jsonl" ]; then
        :
      else
        printf ','
      fi
      printf '{"id":%s,"commit_id":"%s","state":"COMMENTED","body":"history"}' \
        "$((7000 + i))" "ccccccccccccccccccccccccccccccccccccccc"
      i=$((i + 1))
    done
  if [ "$shape" = "truncated" ]; then
    # The document opened and the records are valid; what is missing is the
    # close. A decoder that fills that in would invent the end of the list.
    printf '\n'
  else
    printf ']\n'
  fi
elif [ "$is_files" -eq 1 ]; then
  # The changed-file inventory. A scenario can decline to render the diff, or
  # report a list long enough or heavy enough to exceed the adapter's ceilings.
  field files_shape; shape=$val
  if [ "$shape" = "unavailable" ]; then
    printf 'gh: the diff is too large to render (HTTP 406)\n' >&2
    exit 1
  fi
  printf '['
  first=1
  # A hostile build script in the inventory: its patch text would remove the
  # file named by `LGWKS_TEST_MARKER` if anything executed it. Nothing does; the
  # inventory is read as data, and this is the oracle that observes it.
  field build_script; build=$val
  if [ "$build" = "1" ]; then
    first=0
    printf '{"filename":"build.rs","status":"added","additions":1,"deletions":0,"patch":"rm -f $LGWKS_TEST_MARKER; echo EXECUTED"}'
  fi
  field filler_files; filler=$val
  i=0
  while [ -n "${filler:-}" ] && [ "$i" -lt "$filler" ] 2>/dev/null; do
    if [ "$first" -eq 0 ]; then printf ','; fi
    first=0
    printf '{"filename":"filler-%s.txt","status":"modified","additions":1,"deletions":0,"patch":"x"}' "$i"
    i=$((i + 1))
  done
  field diff_bytes; bytes=$val
  if [ -n "${bytes:-}" ] && [ "$bytes" -gt 0 ] 2>/dev/null; then
    if [ "$first" -eq 0 ]; then printf ','; fi
    first=0
    printf '{"filename":"big.rs","status":"modified","additions":1,"deletions":0,"patch":"'
    # `bytes` bytes of patch text as one JSON string. No newline: a raw newline
    # inside a JSON string is invalid, and the adapter would refuse the document
    # rather than exceed the byte ceiling this scenario is about.
    head -c "$bytes" /dev/zero | tr '\0' 'P'
    printf '"}'
  fi
  printf ']\n'
else
  field snapshot_shape; shape=$val
  if [ "$shape" = "moved" ]; then
    # GitHub's renamed-repository answer: a move object naming the canonical
    # location, which the adapter reports rather than silently following.
    printf '{"message":"Moved Permanently","documentation_url":"https://docs.github.com/rest","url":"https://api.github.com/repos/acme/newrepo/pulls/7"}\n'
  else
    # The pull-request read. `head_after_first` models a head that moves between
    # two reads: the first read reports `head_sha`, and every read after it
    # reports `head_after_first` — so a freshness check comparing the two reads
    # sees exactly the change it is meant to catch. The counter is bumped with a
    # single append per call, so two concurrent runs cannot interleave it.
    field head_sha; head=$val
    field head_after_first; moved=$val
    # Only a scenario that names a second head pays for the read counter: the
    # count decides whether this is the first read or a later one, and when no
    # second head is configured the answer is never consulted. The count is the
    # builtin byte count of the appends (see `creates` above), and it still
    # bumps with one atomic append per read, so two concurrent reads cannot
    # interleave it.
    if [ -n "${moved:-}" ]; then
      printf 'x' >> "$dir/reads"
      read_raw=""
      IFS= read -r read_raw < "$dir/reads"
      read_count=${#read_raw}
      if [ "$read_count" -gt 1 ]; then head="$moved"; fi
    fi
    # GitHub's shape: the commits are nested under `head` and `base`.
    field base_sha; base=$val
    printf '{"number":7,"head":{"ref":"feature","sha":"%s"},"base":{"ref":"main","sha":"%s"}}\n' \
      "$head" "$base"
  fi
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
    /// How many filler reviews the receiver reports on a review read, over and
    /// above whatever it actually applied.
    ///
    /// This is how the review-ceiling probe builds a pull request whose review
    /// history is longer than the adapter will decode. A real one arrives from
    /// `--paginate` following page after page; the fake emits the same *shape*
    /// in one document, because what is under test is the adapter's refusal to
    /// treat a clean decode as proof of completeness.
    pub filler_reviews: u32,
    /// What the review read emits instead of a JSON list: `""` for a real list,
    /// `garbage` for something that is not JSON, or `truncated` for a JSON
    /// document whose closing bracket was lost.
    pub reviews_shape: &'static str,
    /// What the pull-request read emits instead of a pull request: `""` for the
    /// real object, or `moved` for a renamed-repository redirect naming the
    /// canonical location.
    pub snapshot_shape: &'static str,
    /// What the changed-file read emits: `""` for a real list, or `unavailable`
    /// for a server that declines to render the diff (a `406`).
    pub files_shape: &'static str,
    /// How many filler files the changed-file read reports, for the file-count
    /// ceiling.
    pub filler_files: u32,
    /// The bytes of patch text one reported file carries, for the byte ceiling.
    pub diff_bytes: u32,
    /// How many inline comments a `partial` create lands, when it lands fewer
    /// than the payload intended.
    pub applied_comments: u32,
    /// Whether the review read is denied with a `403` permission answer.
    pub deny_reads: bool,
    /// Whether the changed-file inventory names a `build.rs` whose patch text
    /// would perform an effect if it were ever executed.
    pub build_script: bool,
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
            created_state: "COMMENTED",
            fail_reads: 0,
            hang_seconds: 0,
            flood_bytes: 0,
            filler_reviews: 0,
            reviews_shape: "",
            snapshot_shape: "",
            files_shape: "",
            filler_files: 0,
            diff_bytes: 0,
            applied_comments: 0,
            deny_reads: false,
            build_script: false,
        }
    }

    /// Make the fake accept a create request and then drop the response, which leaves the caller unsure whether the review exists.
    #[must_use]
    pub fn accept_then_drop(mut self) -> Self {
        self.create = "accept_then_drop";
        self
    }

    /// Make the fake refuse every create request outright, so the caller must report a refusal with nothing published.
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

    /// Set the body the fake reports for a review it created, so a test can compare it with the body that was sent.
    #[must_use]
    pub fn created_body(mut self, body: &str) -> Self {
        body.clone_into(&mut self.created_body);
        self
    }

    /// Make the pull request's head commit become `head` after the first read, which simulates a push landing mid-review.
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

    /// The receiver reports `count` extra reviews beyond the ones it applied.
    ///
    /// Used to push a pull request's review history past the adapter's declared
    /// ceiling. The point of the probe is the *refusal*, so the filler needs no
    /// particular body: it only has to be a review the adapter would have had to
    /// read to call the list complete.
    #[must_use]
    pub fn with_filler_reviews(mut self, count: u32) -> Self {
        self.filler_reviews = count;
        self
    }

    /// The review read answers with something other than a JSON list.
    ///
    /// `garbage` is not JSON at all; `truncated` is a JSON document whose
    /// closing bracket was lost, which is the shape a truncated stream takes
    /// when the client managed to emit valid-looking JSON first.
    #[must_use]
    pub fn answers_reviews_with(mut self, shape: &'static str) -> Self {
        self.reviews_shape = shape;
        self
    }

    /// The receiver records the create as an unsubmitted draft and drops the
    /// response.
    ///
    /// GitHub creates a review in `PENDING` when the request names no event. The
    /// effect lands (the review exists) and the response is lost, which is the
    /// state a reconciliation must report as a draft, never as a publication and
    /// never as no effect.
    #[must_use]
    pub fn records_pending_draft(mut self) -> Self {
        self.create = "pending";
        self
    }

    /// The receiver records the create with only `applied` of its inline
    /// comments and drops the response.
    ///
    /// The submitted review is real and about the right commit, but fewer
    /// comments landed than intended — the partial submission PR-08 names.
    #[must_use]
    pub fn records_partial_comments(mut self, applied: u32) -> Self {
        self.create = "partial";
        self.applied_comments = applied;
        self
    }

    /// The repository answers with a renamed-repository redirect.
    #[must_use]
    pub fn repository_moved(mut self) -> Self {
        self.snapshot_shape = "moved";
        self
    }

    /// The changed-file read answers with a server that declines to render the
    /// diff (a `406`).
    #[must_use]
    pub fn diff_unavailable(mut self) -> Self {
        self.files_shape = "unavailable";
        self
    }

    /// The changed-file read reports `count` filler files.
    #[must_use]
    pub fn with_filler_files(mut self, count: u32) -> Self {
        self.filler_files = count;
        self
    }

    /// One reported file carries `bytes` of patch text.
    #[must_use]
    pub fn with_diff_bytes(mut self, bytes: u32) -> Self {
        self.diff_bytes = bytes;
        self
    }

    /// The review read answers with a `403` permission refusal.
    ///
    /// The shape `gh` prints when a credential cannot reach the resource, which
    /// the adapter reports as its own typed permission failure rather than as a
    /// transport outage.
    #[must_use]
    pub fn deny_reads(mut self) -> Self {
        self.deny_reads = true;
        self
    }

    /// The changed-file inventory names a `build.rs` that would perform an
    /// effect if anything ever executed its patch text.
    ///
    /// The effect is a shell command that removes the file named by
    /// `LGWKS_TEST_MARKER`, so a non-execution oracle can observe whether the
    /// review path ran it. Nothing here runs it: the inventory is read as data.
    #[must_use]
    pub fn hostile_build_script(mut self) -> Self {
        self.build_script = true;
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
             \"fail_reads\":{fail_reads},\"hang_seconds\":{hang},\"flood_bytes\":{flood},\
             \"filler_reviews\":{filler},\"reviews_shape\":\"{shape}\",\
             \"snapshot_shape\":\"{snapshot}\",\"files_shape\":\"{files}\",\
             \"filler_files\":{filler_files},\"diff_bytes\":{diff_bytes},\
             \"applied_comments\":{applied},\"deny_reads\":{deny},\
             \"build_script\":{build_script}}}\n",
            head = self.head,
            create = self.create,
            body = self.created_body,
            state = self.created_state,
            fail_reads = self.fail_reads,
            hang = self.hang_seconds,
            flood = self.flood_bytes,
            filler = self.filler_reviews,
            shape = self.reviews_shape,
            snapshot = self.snapshot_shape,
            files = self.files_shape,
            filler_files = self.filler_files,
            diff_bytes = self.diff_bytes,
            applied = self.applied_comments,
            deny = u8::from(self.deny_reads),
            build_script = u8::from(self.build_script),
        )
    }
}
