"""The prompt skeleton, the closed-book sandbox and the model CLI call for the authoring bench (#270)."""

import json
import os
import pathlib
import re
import shutil
import sys
import time

from harness import API_DIR, REPO, TASKS_DIR, run_group
from opencode import NO_TOOLS, is_opencode, opencode_argv, opencode_env, parse_opencode, wall_line

#: The lexicon-only arm's whole context is the generated one-page lexicon
#: (#385), read from where the generator writes it rather than from a copy that
#: could drift from the words the parser dispatches on (#270, SL-8).
LEXICON_SHEET = REPO / "docs" / "script-lexicon.md"

FRAMING = (
    "You are authoring one Rust library crate. Read the task and the API sheet "
    "below. You may use only the items on the API sheet plus `std` and the "
    "harness crate `ai_task_support`. Reply with exactly one fenced ```rust "
    "block containing the complete contents of `src/lib.rs`, and nothing else."
)

#: The five required user profiles, as fixed persona preambles.
#:
#: Each is prepended to the SAME prompt skeleton — same task, same API sheet,
#: same framing, same repair budget, same sandbox — so the only thing that
#: differs between two cells of the same (model, api, task) is the sentence
#: below. The profiles are fixed text and never generated, because a profile
#: that varied per trial would be a second uncontrolled variable and the
#: comparison would be measuring the draw rather than the profile.
#:
#: `agent` is deliberately the last instruction in the preamble it belongs to:
#: the framing that follows it ("reply with exactly one fenced ```rust block")
#: is the same for every profile, so `agent` cannot be given a shorter reply
#: contract than the others without changing more than the persona.
PROFILES = {
    "first-time": (
        "You have never used this crate's orchestration API before. Read the API "
        "sheet literally and follow it exactly as written. Where it names an item "
        "or a signature, use that item with that signature rather than reaching "
        "for something you remember from elsewhere. If something is not on the "
        "sheet, it is not available to you."
    ),
    "expert-hurry": (
        "You know this domain well and you are in a hurry. You are looking for "
        "the shortest thing that might work, and you will not re-read the API "
        "sheet's prose a second time. Skim it, write the minimal correct answer, "
        "and stop."
    ),
    "anxious": (
        "You do not trust this API to clean up after itself. Assume every "
        "resource you start stays live unless you explicitly stop it, and assume "
        "every fallible call needs your own error handling rather than an "
        "implicit one. Be explicit about cleanup and about every error path."
    ),
    "misuser": (
        "You are confident but mistaken. The most plausible mistake here is to "
        "write the obvious first solution, which quietly ignores the deadline or "
        "the path that runs when the work is dropped. Write it anyway; when the "
        "compiler or the oracle tells you it is wrong, read what it says and fix "
        "it from that evidence."
    ),
    "agent": (
        "You are an AI coding agent working on an automated pipeline. Output only "
        "code. Do not explain, do not ask questions, and do not produce anything "
        "outside the single required fenced block."
    ),
}

#: The profiles a run covers when `--profiles` is not given.
DEFAULT_PROFILES = tuple(PROFILES)

RUST_FENCE = re.compile(r"```rust[ \t]*\r?\n(.*?)```", re.DOTALL)
ANY_FENCE = re.compile(r"```[a-zA-Z0-9_+-]*[ \t]*\r?\n(.*?)```", re.DOTALL)


# ── prompt and model I/O ─────────────────────────────────────────────────────


def read_task_prompt(task: str) -> str:
    return (TASKS_DIR / task / "prompt.md").read_text()


def read_api_sheet(api: str) -> str:
    """The sheet arm `api` authors from; the `lexicon` arm's is the lexicon itself."""
    if api == "lexicon":
        return LEXICON_SHEET.read_text()
    return (API_DIR / f"{api}.md").read_text()


def profile_preamble(profile: str) -> str:
    """The fixed persona text for `profile`, or an empty string for no profile."""
    return PROFILES.get(profile, "")


def full_prompt(task: str, api: str, profile: str = "") -> str:
    """The one prompt skeleton, with `profile`'s preamble in front of it.

    The preamble goes first and the framing last, so the reply contract — one
    fenced block, nothing else — is the final instruction the model reads and
    every profile answers under the same one.
    """
    preamble = profile_preamble(profile)
    parts = []
    if preamble:
        parts.append("=== ROLE ===\n\n" + preamble)
    parts.append("=== TASK ===\n\n" + read_task_prompt(task))
    parts.append("=== API SHEET ===\n\n" + read_api_sheet(api))
    return FRAMING + "\n\n" + "\n\n".join(parts) + "\n"


def sbpl_string(path: str) -> str:
    """A path as an SBPL string literal."""
    return '"' + path.replace("\\", "\\\\").replace('"', '\\"') + '"'


def closed_book_profile(scratch: pathlib.Path, work: pathlib.Path) -> str:
    """The `sandbox-exec` profile that makes a trial closed-book.

    The model CLI runs with tools, so without this it can read the hidden
    oracle, the reference solutions, the crate source, earlier sessions and the
    sibling trials' passing answers — the first matrix did exactly that. The
    profile denies the home directory, `/private/tmp` and the run's work
    directory, then re-allows only the CLI's own credentials and settings and
    this trial's scratch directory. Paths are resolved, because the kernel
    matches the real path (`/private/var/...`), never the `$TMPDIR` spelling.
    """
    home = os.path.realpath(os.path.expanduser("~"))
    config = os.path.join(home, ".commandcode")
    withheld = ("projects", "file-history", "history.jsonl", "plans", "scratchpad", "skills")
    return "\n".join(
        [
            "(version 1)",
            "(allow default)",
            "(deny file-read* file-write* (subpath {}) (subpath {}) (subpath {}))".format(
                sbpl_string(home), sbpl_string("/private/tmp"), sbpl_string(os.path.realpath(work))
            ),
            "(allow file-read-metadata (literal {}))".format(sbpl_string(home)),
            "(allow file-read* file-write* (subpath {}))".format(sbpl_string(config)),
            "(deny file-read* file-write* {})".format(
                " ".join(
                    "(subpath {})".format(sbpl_string(os.path.join(config, name)))
                    for name in withheld
                )
            ),
            "(allow file-read* file-write* (subpath {}))".format(
                sbpl_string(os.path.realpath(scratch))
            ),
        ]
    )


def call_model(
    cmd: str,
    model: str,
    prompt: str,
    scratch: pathlib.Path,
    work: pathlib.Path,
    timeout: int = 900,
):
    """Invoke the model CLI once, closed-book. Returns (stdout, stderr)."""
    if sys.platform != "darwin" or shutil.which("sandbox-exec") is None:
        raise RuntimeError(
            "a model trial must be closed-book, and this runner enforces that with "
            "macOS sandbox-exec, which this host lacks"
        )
    profile = closed_book_profile(scratch, work)
    if is_opencode(cmd):
        # OpenCode runs under the same profile with every home it touches moved
        # into scratch (`opencode.py`), so the sandbox is unchanged across CLIs.
        started = time.monotonic()
        done = run_group(
            ["sandbox-exec", "-p", profile, *opencode_argv(cmd, model)],
            scratch,
            opencode_env(scratch),
            timeout,
            stdin_text=NO_TOOLS + prompt,
        )
        elapsed = (time.monotonic() - started) * 1000.0
        return done.stdout + "\n" + wall_line(elapsed) + "\n", done.stderr
    # Read-only plan mode, never a stripped permission check: the whole prompt
    # arrives on stdin and the reply is one fenced block, so an author needs no
    # tool that writes or runs anything. The 2026-10-03/04 runs passed `--yolo`;
    # the estate refuses an agent launched with its checks stripped, and a trial
    # that needed one would be measuring something other than authoring.
    argv = [
        "sandbox-exec",
        "-p",
        profile,
        cmd,
        "-p",
        "--permission-mode",
        "plan",
        "-t",
        "--skip-onboarding",
        "--no-auto-update",
        "--no-session",
        "--no-skills",
        "-m",
        model,
        "--effort",
        "high",
        "--max-turns",
        "6",
        "--output-format",
        "json",
    ]
    done = run_group(argv, scratch, None, timeout, stdin_text=prompt)
    return done.stdout, done.stderr


def parse_ndjson(raw: str):
    """Parse the final NDJSON result line: (final_text, tokens_in, tokens_out, ms)."""
    events = parse_opencode(raw)
    if events is not None:
        return events
    for line in reversed(raw.splitlines()):
        line = line.strip()
        if not line.startswith("{"):
            continue
        try:
            record = json.loads(line)
        except json.JSONDecodeError:
            continue
        if not isinstance(record, dict):
            continue
        if "finalText" in record or record.get("type") == "result":
            usage = record.get("usage") or {}
            tokens_in = usage.get("inputTokens")
            tokens_out = usage.get("outputTokens")
            duration = record.get("durationMs")
            return (
                record.get("finalText") or "",
                int(tokens_in) if isinstance(tokens_in, (int, float)) else 0,
                int(tokens_out) if isinstance(tokens_out, (int, float)) else 0,
                int(duration) if isinstance(duration, (int, float)) else 0,
            )
    return "", 0, 0, 0


def extract_rust_block(text: str) -> str:
    """The last ```rust block, or the last block of any language, or empty."""
    blocks = RUST_FENCE.findall(text)
    if not blocks:
        blocks = ANY_FENCE.findall(text)
    return blocks[-1].rstrip() + "\n" if blocks else ""


def repair_prompt(original: str, code: str, errors: str) -> str:
    """The re-prompt after a failed compile: original prompt, code, first 80 errors.

    A reply with no fenced block is a failed attempt too: it is re-prompted
    under the same budget rather than compiled as an empty crate.
    """
    if not code:
        return (
            original
            + "\n\n=== PREVIOUS ATTEMPT ===\n\n"
            "Your previous reply contained no fenced ```rust block. Reply with "
            "exactly one fenced ```rust block containing the complete `src/lib.rs`, "
            "and nothing else.\n"
        )
    error_lines = "\n".join(errors.splitlines()[:80])
    return (
        original
        + "\n\n=== PREVIOUS ATTEMPT ===\n\n"
        "Your previous attempt did not compile. Here is the current complete "
        "`src/lib.rs`:\n\n```rust\n" + code + "\n```\n\n"
        "The compiler reported (first 80 lines):\n\n```\n" + error_lines + "\n```\n\n"
        "Reply with exactly one fenced ```rust block containing the corrected "
        "complete `src/lib.rs`, and nothing else.\n"
    )
