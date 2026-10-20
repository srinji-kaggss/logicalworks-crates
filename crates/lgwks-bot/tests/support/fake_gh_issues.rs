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
//! | `refuse` | refuses the create without holding it (HTTP 422) |
//! | `deny` | refuses every call's credential (HTTP 403) |
//! | `denycreate` | answers reads, refuses the create's credential (HTTP 403) |
//! | `lag` | as `accept`, but the two listings after a create omit that issue |
//! | `stale` | as `accept`, but its listing never shows its newest issue |
//!
//! Each `owner/repo` has its own store, so tests that use distinct owners
//! never see each other's issues however they are scheduled.

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
# Every answer is cut to number, page and title inside `gh`, as `--jq` does,
# so a call without the projection would print bodies past the capture.
if [ -z "$jq" ]; then
  printf 'fake gh: every call must pass --jq\n' >&2
  exit 64
fi
# A record's body is its last field, so the projection drops the tail.
project() {
  printf '%s}' "${1%%,\"body\":*}"
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
  payload=""
  IFS= read -r payload < "$input"
  # One atomic append per create; its byte count is the issue number.
  printf 'x' >> "$dir/count-$owner-$repo"
  raw=""
  IFS= read -r raw < "$dir/count-$owner-$repo"
  number=${#raw}
  record="{\"number\":$number,\"html_url\":\"https://github.com/$owner/$repo/issues/$number\",${payload#\{}"
  printf '%s\n' "$record" >> "$store"
  # `lag` hides this issue from the next two listings.
  if [ "$repo" = "lag" ]; then printf 'xx' > "$dir/lag-$owner-$repo"; fi
  if [ "$repo" = "drop" ]; then
    printf 'connection reset by peer\n' >&2
    exit 7
  fi
  project "$record"
  printf '\n'
  exit 0
fi

tail=${jq#*<!-- lgwks-issue:}
wanted="<!-- lgwks-issue:${tail%% -->*} -->"

# A read of one issue by number: projected when it carries the trailer.
case "$rest" in
  */issues/[0-9]*)
    number=${rest##*/}
    if [ -s "$store" ]; then
      while IFS= read -r record; do
        case "$record" in
          "{\"number\":$number,"*"$wanted"*) project "$record"; printf '\n'; exit 0 ;;
        esac
      done < "$store"
    fi
    printf '{"number":0}\n'
    exit 0
    ;;
esac

# A listing, filtered to the trailer the `--jq` names: newest first, as the
# endpoint sorts `created desc`. GitHub's listing lags a create: `lag` omits
# its newest issue from the two listings after a create, `stale` from all.
held=0
if [ -s "$store" ]; then
  while IFS= read -r record; do held=$((held + 1)); done < "$store"
fi
lagging="$dir/lag-$owner-$repo"
if [ "$repo" = "lag" ] && [ -s "$lagging" ]; then
  held=$((held - 1))
  pending=""
  IFS= read -r pending < "$lagging"
  printf '%s' "${pending#x}" > "$lagging"
fi
if [ "$repo" = "stale" ]; then held=$((held - 1)); fi
out=""
seen=0
if [ -s "$store" ]; then
  while IFS= read -r record; do
    seen=$((seen + 1))
    if [ "$seen" -gt "$held" ]; then break; fi
    case "$record" in
      *"$wanted"*)
        kept=$(project "$record")
        if [ -z "$out" ]; then out="$kept"; else out="$kept,$out"; fi
        ;;
    esac
  done < "$store"
fi
printf '[%s]\n' "$out"
exit 0
"#;
