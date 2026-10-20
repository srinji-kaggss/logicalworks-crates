//! A fake `gh` that holds issues, for the `gh::issue` action's tests.
//!
//! Nothing here mocks a Rust function: the action forks this script through
//! the same supervised runner it forks the real client through, stages its
//! payload as a file, and parses what the script prints. The script records
//! one argv line per call before it does anything else, so a test counts the
//! creates the action *issued*, including one whose answer was lost.
//!
//! The repository name is the scenario, so a registry constructor — a plain
//! function pointer — needs no state to choose one:
//!
//! | Repository | What the receiver does |
//! |---|---|
//! | `accept` | holds the issue and answers with it |
//! | `drop` | holds the issue, then loses the answer (exit 7) |
//! | `droplag-N` | as `drop`, and its listings hide the issue for N reads |
//! | `kill` | holds the issue, then `SIGKILL`s itself before answering |
//! | `kill-N` | as `kill`, and its listings hide the issue for N reads |
//! | `refuse` | rejects the create without holding it (HTTP 422) |
//! | `vanish` | loses the create's answer (exit 7) without holding the issue |
//! | `deny` | refuses every call's credential (HTTP 403) |
//! | `denycreate` | answers reads, refuses the create's credential (HTTP 403) |
//! | `lag` | as `accept`, but the two listings after a create omit that issue |
//! | `lag-N` | as `lag`, with N listings omitting the created issue |
//! | `stale` | as `accept`, but its listing never shows its newest issue |
//!
//! It models what GitHub is: one number sequence per repository, an issue's
//! own address that answers at once (`404` for a number not yet used), and a
//! listing that trails the creates. A created issue's lag is the issue's own,
//! spent by every listing read whatever key it scans, so the listing hides
//! the newest issues and never an older one while showing a newer.
//!
//! A lost answer and a lagging listing are two independent faults, so they are
//! two axes of the same name: `drop` and `kill` differ in *how* the answer is
//! lost (a normal failure exit against a real signal death between the POST and
//! its reply), and `-N` names how many listing reads still hide the issue. Each
//! `owner/repo` has its own store, so tests that use distinct owners never see
//! each other's issues however they are scheduled.

use std::path::PathBuf;
use std::sync::LazyLock;

use lgwks_bot::domain::gh::{Gh, IssueDraft, OpenIssue, Repository};
use lgwks_bot::{Action, BotError};

/// The fake's directory, installed once per test process.
///
/// Under the target's own temporary directory, so a run leaves nothing
/// outside `target/`.
static FAKE: LazyLock<Result<PathBuf, String>> =
    LazyLock::new(|| install().map_err(|cause| cause.to_string()));

/// Write the script into a fresh directory and make it executable.
fn install() -> std::io::Result<PathBuf> {
    use std::os::unix::fs::PermissionsExt as _;

    let tag = lgwks_std::random::bytes::<8>()
        .map(u64::from_le_bytes)
        .map_err(std::io::Error::other)?;
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("fake-gh-issues-{tag:016x}"));
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join("argv.log"), b"")?;
    let script = dir.join("gh");
    std::fs::write(&script, FAKE_GH_ISSUES)?;
    let mut permissions = std::fs::metadata(&script)?.permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&script, permissions)?;
    Ok(dir)
}

/// The fake's directory, or why it could not be installed.
fn dir() -> Result<&'static PathBuf, String> {
    match *FAKE {
        Ok(ref dir) => Ok(dir),
        Err(ref cause) => Err(cause.clone()),
    }
}

/// The registry constructor that binds `gh::issue` to this fake.
pub fn issue_on_fake(target: &str) -> Result<Action, BotError> {
    let unusable = |cause: String| BotError::IncompleteSpec {
        field: "target",
        cause,
    };
    let script = dir().map_err(unusable)?.join("gh");
    let Some(program) = script.to_str() else {
        lgwks_std::trace::debug!(path = ?script, "fake_gh_issues: the fake's path is not UTF-8");
        return Err(unusable(String::from("the fake's path is not UTF-8")));
    };
    let repository = Repository::new(target).map_err(|refused| unusable(refused.to_string()))?;
    Ok(Action::new(OpenIssue::new(
        Gh::new(repository).program(program),
    )))
}

/// Every call the fake recorded against `owner`, in order, one argv each.
pub fn calls_on(owner: &str) -> Result<Vec<Vec<String>>, Box<dyn std::error::Error>> {
    let needle = format!("repos/{owner}/");
    let text = std::fs::read_to_string(dir()?.join("argv.log"))?;
    Ok(text
        .lines()
        .map(|line| {
            line.split('\t')
                .filter(|arg| !arg.is_empty())
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .filter(|argv| argv.iter().any(|arg| arg.starts_with(&needle)))
        .collect())
}

/// Put `draft` into `owner/repo`'s store as issue `number`, as if an earlier
/// run had opened it; the store line is the one a create writes.
pub fn hold(
    owner: &str,
    repo: &str,
    number: u64,
    draft: &IssueDraft,
) -> Result<(), Box<dyn std::error::Error>> {
    use std::io::Write as _;

    let record = format!(
        "{{\"number\":{number},\"html_url\":\"https://github.com/{owner}/{repo}/issues/{number}\",\"title\":{},\"body\":{}}}\n",
        lgwks_std::json::to_string(draft.title())?,
        lgwks_std::json::to_string(draft.body())?
    );
    let path = dir()?.join(format!("issues-{owner}-{repo}.jsonl"));
    let mut store = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    store.write_all(record.as_bytes())?;
    Ok(())
}

/// How many creates the action issued against `owner`.
pub fn creates_on(owner: &str) -> Result<usize, Box<dyn std::error::Error>> {
    Ok(calls_on(owner)?
        .iter()
        .filter(|argv| argv.iter().any(|arg| arg == "POST"))
        .count())
}

/// How many issues the fake holds for `owner/repo`, one store line each.
///
/// A create's POST count is what a duplicate raises; the store is what a
/// duplicate *leaves behind*, and the two can differ if a create landed and
/// was never acknowledged.
pub fn held_on(owner: &str, repo: &str) -> Result<usize, Box<dyn std::error::Error>> {
    let path = dir()?.join(format!("issues-{owner}-{repo}.jsonl"));
    match std::fs::read_to_string(&path) {
        Ok(text) => Ok(text.lines().filter(|line| !line.is_empty()).count()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(error) => Err(error.into()),
    }
}

/// The fake client. Bash where the host has it, as the review fake chooses.
const FAKE_GH_ISSUES: &str = r#"#!/bin/bash
# A fake `gh api` holding issues. Records argv, then answers from its store.
set -u
case "$0" in
  */*) dir=${0%/*} ;;
  *) dir=. ;;
esac
tab='	'
line=""
for arg in "$@"; do
  line="$line$arg$tab"
done
printf '%s\n' "$line" >> "$dir/argv.log"

method=GET
input=""
path=""
jq=""
previous=""
for arg in "$@"; do
  if [ "$previous" = "--method" ]; then method="$arg"; fi
  if [ "$previous" = "--input" ]; then input="$arg"; fi
  if [ "$previous" = "--jq" ]; then jq="$arg"; fi
  case "$arg" in repos/*) path="$arg" ;; esac
  previous="$arg"
done
# Every answer is cut to number, page, title and a keyed flag inside `gh`, as
# `--jq` does, so a call without the projection would print bodies past the
# capture.
if [ -z "$jq" ]; then
  printf 'fake gh: every call must pass --jq\n' >&2
  exit 64
fi
tail=${jq#*<!-- lgwks-issue:}
wanted="<!-- lgwks-issue:${tail%% -->*} -->"
# A record's body is its last field, so the projection drops the tail and
# says whether the body carried the scanned key's trailer.
project() {
  case "$1" in
    *"$wanted"*) printf '%s,"keyed":true}' "${1%%,\"body\":*}" ;;
    *) printf '%s,"keyed":false}' "${1%%,\"body\":*}" ;;
  esac
}
rest=${path#repos/}
owner=${rest%%/*}
rest=${rest#*/}
repo=${rest%%/*}
repo=${repo%%\?*}
store="$dir/issues-$owner-$repo.jsonl"

if [ "$repo" = "deny" ]; then
  printf 'gh: Resource not accessible by integration (HTTP 403)\n' >&2
  exit 1
fi

if [ "$method" = "POST" ]; then
  if [ "$repo" = "denycreate" ]; then
    printf 'gh: Resource not accessible by integration (HTTP 403)\n' >&2
    exit 1
  fi
  if [ "$repo" = "refuse" ]; then
    printf 'gh: Validation Failed (HTTP 422)\n' >&2
    exit 1
  fi
  if [ "$repo" = "vanish" ]; then
    printf 'connection reset by peer\n' >&2
    exit 7
  fi
  payload=""
  IFS= read -r payload < "$input"
  # One atomic append per create; its byte count is the issue number.
  printf 'x' >> "$dir/count-$owner-$repo"
  raw=""
  IFS= read -r raw < "$dir/count-$owner-$repo"
  number=${#raw}
  record="{\"number\":$number,\"html_url\":\"https://github.com/$owner/$repo/issues/$number\",${payload#\{}"
  printf '%s\n' "$record" >> "$store"
  # `lag` hides this issue from the next two listings; `lag-N` from the next N.
  # A lost answer carries a lag the same way: `droplag-N` and `kill-N`.
  lag=0
  case "$repo" in
    lag) lag=2 ;;
    lag-*) lag=${repo#lag-} ;;
    droplag-*) lag=${repo#droplag-} ;;
    kill-*) lag=${repo#kill-} ;;
  esac
  case "$lag" in ''|*[!0-9]*) lag=0 ;; esac
  if [ "$lag" -gt 0 ]; then
    printf '%s' "$lag" > "$dir/lag-$owner-$repo-$number"
  fi
  # The answer is lost after the issue is held. `drop` exits non-zero with a
  # transport error; `kill` dies by SIGKILL, as a `kill -9` of the child
  # between the POST and its reply does, so the parent reads a signal death and
  # no exit code at all.
  case "$repo" in
    drop|droplag-*)
      printf 'connection reset by peer\n' >&2
      exit 7
      ;;
    kill|kill-*)
      kill -9 $$
      ;;
  esac
  project "$record"
  printf '\n'
  exit 0
fi

# A read of one issue by number answers at once, lag or not: projected when
# the number exists, `404` when it does not exist yet.
case "$rest" in
  */issues/[0-9]*)
    number=${rest##*/}
    if [ -s "$store" ]; then
      while IFS= read -r record; do
        case "$record" in
          "{\"number\":$number,"*) project "$record"; printf '\n'; exit 0 ;;
        esac
      done < "$store"
    fi
    printf 'gh: Not Found (HTTP 404)\n' >&2
    exit 1
    ;;
esac

# A listing: the newest number it reaches and the records carrying the
# trailer the `--jq` names, newest first, as the endpoint sorts `created
# desc`. A record whose lag file still counts is hidden, and the read spends
# one of its count. `stale` never lists its newest record.
top=0
if [ "$repo" = "stale" ] && [ -s "$store" ]; then
  while IFS= read -r record; do
    n=${record#\{\"number\":}; n=${n%%,*}
    if [ "$n" -gt "$top" ]; then top=$n; fi
  done < "$store"
fi
newest=0
out=""
if [ -s "$store" ]; then
  while IFS= read -r record; do
    n=${record#\{\"number\":}; n=${n%%,*}
    lagging="$dir/lag-$owner-$repo-$n"
    left=0
    if [ -s "$lagging" ]; then IFS= read -r left < "$lagging"; fi
    case "$left" in ''|*[!0-9]*) left=0 ;; esac
    if [ "$left" -gt 0 ]; then
      left=$((left - 1))
      if [ "$left" -eq 0 ]; then : > "$lagging"; else printf '%s' "$left" > "$lagging"; fi
      continue
    fi
    if [ "$repo" = "stale" ] && [ "$n" -eq "$top" ]; then continue; fi
    if [ "$n" -gt "$newest" ]; then newest=$n; fi
    case "$record" in
      *"$wanted"*)
        kept=$(project "$record")
        if [ -z "$out" ]; then out="$kept"; else out="$kept,$out"; fi
        ;;
    esac
  done < "$store"
fi
printf '{"newest":%s,"keyed":[%s]}\n' "$newest" "$out"
exit 0
"#;
