"""The OpenCode CLI as the authoring bench's model caller (#270): argv, a per-trial home, and its JSON events."""

import json
import os
import pathlib

#: Every directory OpenCode reads or writes, pointed inside the trial's scratch.
#:
#: OpenCode reads `~/.opencode/opencode.json`, the XDG config, data, state and
#: cache homes, and `$TMPDIR`. The closed-book sandbox denies the real home and
#: `/private/tmp`, so a CLI left on its defaults fails at start-up, and one given
#: the real homes would read the user's global rules (20k tokens of them on this
#: host), earlier sessions and the sibling trials' answers in its session store.
#: Inside scratch, the only path the sandbox re-allows, each trial starts with no
#: rules, no session and no other trial's text.
HOMES = {
    "HOME": "home",
    "TMPDIR": "tmp",
    "XDG_CONFIG_HOME": "config",
    "XDG_DATA_HOME": "data",
    "XDG_STATE_HOME": "state",
    "XDG_CACHE_HOME": "cache",
}


#: The event type of the line `call_model` appends after the CLI exits.
WALL = "bench_wall"


def wall_line(ms: float) -> str:
    """The line recording the CLI process's measured wall time."""
    return json.dumps({"type": WALL, "ms": round(ms)})


def is_opencode(cmd: str) -> bool:
    """Whether `cmd` names the OpenCode CLI rather than the default `cmd` CLI."""
    return pathlib.Path(cmd).name == "opencode"


def opencode_argv(cmd: str, model: str) -> list:
    """One non-interactive OpenCode call: the prompt arrives on stdin.

    `--pure` loads no external plugin. The Zen free tier answers 403 to a client
    whose prompt, tool or permission set differs from the stock one, so the arms
    share OpenCode's stock agent prompt, `CONFIG` only caps its steps, and the
    sandbox, not a permission list, is what keeps the author closed-book.
    """
    return [cmd, "run", "--pure", "--format", "json", "-m", model]


#: The config each trial writes. The Zen free tier refuses (403) a changed prompt
#: or tool list but accepts a step cap; measured 2026-10-12, the cap alone did not
#: stop a tool loop, so `NO_TOOLS` is what keeps a call to one step.
CONFIG = '{"$schema":"https://opencode.ai/config.json","agent":{"build":{"steps":1}}}\n'

#: Prepended to every OpenCode prompt, the same text for every arm.
#:
#: The stock agent has live tools and the sandbox denies them everything, so an
#: author that reaches for them spends tokens learning nothing: one 2026-10-12
#: trial sent 597,552 input tokens to answer one task. With this line every model
#: measured answered in one step from the prompt alone.
NO_TOOLS = "Answer from the text below alone. Do not call any tool.\n\n"


def opencode_env(scratch: pathlib.Path) -> dict:
    """The process environment with every OpenCode home inside `scratch`.

    `PWD` is reset too: OpenCode stats the inherited `$PWD`, which names the
    runner's own directory, and the sandbox denies it.
    """
    environment = dict(os.environ)
    environment["PWD"] = str(scratch)
    root = scratch / ".opencode"
    for variable, name in HOMES.items():
        path = root / name
        path.mkdir(parents=True, exist_ok=True)
        environment[variable] = str(path)
    (root / "config" / "opencode").mkdir(parents=True, exist_ok=True)
    (root / "config" / "opencode" / "opencode.json").write_text(CONFIG)
    return environment


def parse_opencode(raw: str):
    """`(final_text, tokens_in, tokens_out, ms)` from OpenCode's JSON events, or `None`.

    `None` means `raw` holds no OpenCode event, so the caller falls back to the
    `cmd` result line. Text is every `text` part in order. Tokens are summed
    over `step_finish` events: input counts the cache reads too, because a cached
    token was still sent as input, and output counts the reasoning tokens, because
    the model generated them. The wall time is the `WALL` line `call_model`
    appends, measured around the whole CLI process: OpenCode's events carry
    millisecond stamps that can all coincide for a short reply, which would
    report a model call as taking no time at all.
    """
    texts = []
    tokens_in = 0
    tokens_out = 0
    wall = 0
    seen = False
    for line in raw.splitlines():
        line = line.strip()
        if not line.startswith("{"):
            continue
        try:
            event = json.loads(line)
        except json.JSONDecodeError:
            continue
        if not isinstance(event, dict):
            continue
        if event.get("type") == WALL:
            wall = int(event.get("ms") or 0)
            continue
        if "sessionID" not in event:
            continue
        seen = True
        part = event.get("part") or {}
        if event.get("type") == "text" and isinstance(part.get("text"), str):
            texts.append(part["text"])
        if event.get("type") == "step_finish":
            tokens = part.get("tokens") or {}
            cache = tokens.get("cache") or {}
            tokens_in += int(tokens.get("input") or 0) + int(cache.get("read") or 0)
            tokens_out += int(tokens.get("output") or 0) + int(tokens.get("reasoning") or 0)
    if not seen:
        return None
    return "\n".join(texts), tokens_in, tokens_out, wall
