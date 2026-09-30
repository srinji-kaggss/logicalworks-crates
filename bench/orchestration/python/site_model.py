"""The site model and probe every Python way shares, per ../README.md.

Only the orchestration differs between ways; this file is the workload and the
counters, written once so the numbers mean the same thing everywhere.
"""

import json
import os
import time

MACHINE = (os.cpu_count() or 1) * 64
SCENARIOS = {
    "throughput": (2, 10_000, MACHINE, 3),
    "failfast": (1, 2_000, MACHINE, 1),
    "cancel": (1, 2_000, MACHINE, 1),
    "storm": (1, 1_000, 100, 5),
    "deadline": (1, 1, 1, 1),
}
TENANTS = ("acme", "globex")


class Transient(Exception):
    """A failure the same call may not repeat."""


class Permanent(Exception):
    """A failure every repeat repeats."""


class Spec:
    """The numbers a scenario fixes."""

    def __init__(self, scenario):
        self.scenario = scenario
        self.tenants, self.items, self.bound, self.attempts = SCENARIOS[scenario]
        self.wait = 0.005 if scenario == "throughput" else 0.0
        self.deadline = 0.1 if scenario == "deadline" else 1.0
        self.cancel_after = 0.010 if scenario == "cancel" else None


class Live:
    """A body's presence: counted in on entry, out on any exit."""

    def __init__(self, probe):
        self.probe = probe
        self.started = time.perf_counter()

    def __enter__(self):
        probe = self.probe
        probe.live += 1
        probe.peak = max(probe.peak, probe.live)
        return self

    def finish(self):
        self.probe.finished += 1
        self.probe.latencies.append(int((time.perf_counter() - self.started) * 1e6))

    def __exit__(self, *_):
        self.probe.live -= 1
        return False


class Site:
    """One call of one attempt for one item, and the counters."""

    def __init__(self, spec, sleep):
        self.spec = spec
        self.sleep = sleep
        self.live = self.peak = self.finished = self.attempts = self.duplicates = 0
        self.latencies = []
        self.served = set()
        self.failed_once = set()

    def enter(self):
        return Live(self)

    async def attempt(self, key, item):
        self.attempts += 1
        scenario = self.spec.scenario
        pause = {"throughput": 1, "storm": 1, "cancel": 50, "deadline": 2_000}.get(
            scenario, (item * 7_919) % 20 + 1
        )
        await self.sleep(pause / 1_000)
        if scenario == "storm":
            raise Transient("the upstream is down")
        if scenario == "failfast" and item == 500:
            raise Permanent("malformed record")
        if scenario == "throughput" and item % 97 == 0 and key not in self.failed_once:
            self.failed_once.add(key)
            raise Transient("the site was busy")
        if key in self.served:
            self.duplicates += 1
        self.served.add(key)
        return item


def quantile(values, permille):
    """The value at `permille` of sorted `values`, as milliseconds."""
    if not values:
        return 0.0
    return values[(len(values) - 1) * permille // 1_000] / 1_000


def first_error(error):
    """The first leaf of an exception group, rendered."""
    while isinstance(error, BaseExceptionGroup):
        error = error.exceptions[0]
    return f"{type(error).__name__}: {error}"


def report(way, site, elapsed, total, error, live_at_return, finished_at_return, live_after_grace):
    """Print the one JSON line every way prints."""
    spec = site.spec
    items = spec.items * spec.tenants
    latencies = sorted(site.latencies)
    print(json.dumps({
        "way": way, "scenario": spec.scenario, "ok": not error, "items": items,
        "total": total if not error else 0, "wall_ms": round(elapsed * 1_000, 3),
        "per_s": int(items / elapsed) if not error else 0,
        "p50_ms": quantile(latencies, 500), "p95_ms": quantile(latencies, 950),
        "p99_ms": quantile(latencies, 990), "bound": spec.bound, "peak_in_flight": site.peak,
        "attempts": site.attempts, "duplicates": site.duplicates,
        "live_at_return": live_at_return, "live_after_grace": live_after_grace,
        "finished_after_return": site.finished - finished_at_return, "error": error,
    }))
