// The site model and probe every Node way shares, per ../README.md. It mirrors
// ../python/site_model.py: only the orchestration differs between ways.

import { availableParallelism } from "node:os";

const MACHINE = availableParallelism() * 64;
const SCENARIOS = {
  throughput: [2, 10_000, MACHINE, 3],
  failfast: [1, 2_000, MACHINE, 1],
  cancel: [1, 2_000, MACHINE, 1],
  storm: [1, 1_000, 100, 5],
  deadline: [1, 1, 1, 1],
};
export const TENANTS = ["acme", "globex"];

export class Transient extends Error {}
export class Permanent extends Error {}

export class Spec {
  constructor(scenario) {
    if (!(scenario in SCENARIOS)) throw new Error(`unknown scenario ${scenario}`);
    this.scenario = scenario;
    [this.tenants, this.items, this.bound, this.attempts] = SCENARIOS[scenario];
    this.wait = scenario === "throughput" ? 5 : 0;
    this.deadline = scenario === "deadline" ? 100 : 1_000;
    this.cancelAfter = scenario === "cancel" ? 10 : null;
  }
}

class Live {
  constructor(site) {
    this.site = site;
    this.started = performance.now();
    site.live += 1;
    site.peak = Math.max(site.peak, site.live);
  }
  finish() {
    this.site.finished += 1;
    this.site.latencies.push(Math.round((performance.now() - this.started) * 1_000));
  }
  exit() {
    this.site.live -= 1;
  }
}

export class Site {
  constructor(spec) {
    this.spec = spec;
    this.live = this.peak = this.finished = this.attempts = this.duplicates = 0;
    this.latencies = [];
    this.served = new Set();
    this.failedOnce = new Set();
  }
  enter() {
    return new Live(this);
  }
  // Count an attempt and say how long it takes, in milliseconds.
  begin(item) {
    this.attempts += 1;
    const pause = { throughput: 1, storm: 1, cancel: 50, deadline: 2_000 }[this.spec.scenario];
    return pause ?? ((item * 7_919) % 20) + 1;
  }
  // The attempt's answer once its time has passed: a value or a throw.
  settle(key, item) {
    const scenario = this.spec.scenario;
    if (scenario === "storm") throw new Transient("the upstream is down");
    if (scenario === "failfast" && item === 500) throw new Permanent("malformed record");
    if (scenario === "throughput" && item % 97 === 0 && !this.failedOnce.has(key)) {
      this.failedOnce.add(key);
      throw new Transient("the site was busy");
    }
    if (this.served.has(key)) this.duplicates += 1;
    this.served.add(key);
    return item;
  }
}

const quantile = (values, permille) =>
  values.length ? values[Math.floor(((values.length - 1) * permille) / 1_000)] / 1_000 : 0;

// Run `body(site, signal)` once per tenant, cancel if the scenario says so,
// and print the one JSON line every way prints.
export async function measure(way, scenario, runTenants) {
  const site = new Site(new Spec(scenario));
  const spec = site.spec;
  const caller = new AbortController();
  const started = performance.now();
  if (spec.cancelAfter !== null) setTimeout(() => caller.abort(new Error("cancelled")), spec.cancelAfter);
  let total = 0;
  let error = "";
  try {
    total = await runTenants(site, TENANTS.slice(0, spec.tenants), caller.signal);
  } catch (failure) {
    error = `${failure?.name ?? "Error"}: ${failure?.message ?? failure}`;
  }
  const elapsed = performance.now() - started;
  const liveAtReturn = site.live;
  const finishedAtReturn = site.finished;
  await new Promise((resolve) => setTimeout(resolve, 200));
  const items = spec.items * spec.tenants;
  const latencies = [...site.latencies].sort((a, b) => a - b);
  console.log(JSON.stringify({
    way, scenario, ok: !error, items, total: error ? 0 : total,
    wall_ms: Math.round(elapsed * 1_000) / 1_000,
    per_s: error ? 0 : Math.floor(items / (elapsed / 1_000)),
    p50_ms: quantile(latencies, 500), p95_ms: quantile(latencies, 950), p99_ms: quantile(latencies, 990),
    bound: spec.bound, peak_in_flight: site.peak, attempts: site.attempts, duplicates: site.duplicates,
    live_at_return: liveAtReturn, live_after_grace: site.live,
    finished_after_return: site.finished - finishedAtReturn, error,
  }));
}
