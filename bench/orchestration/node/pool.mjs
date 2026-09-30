// Node, written as a careful author would with the standard library: a
// bounded worker pool, AbortController for fail-fast and cancellation,
// AbortSignal.timeout per attempt.
//
//   node pool.mjs <scenario>

import { setTimeout as sleep } from "node:timers/promises";
import { Transient, measure } from "./site_model.mjs";

// BEGIN pool
async function one(site, tenant, item, signal) {
  const live = site.enter();
  try {
    const key = `${tenant}/${item}`;
    for (let attempt = 1; ; attempt += 1) {
      try {
        const deadline = AbortSignal.any([signal, AbortSignal.timeout(site.spec.deadline)]);
        await sleep(site.begin(item), undefined, { signal: deadline });
        const value = site.settle(key, item);
        live.finish();
        return value;
      } catch (error) {
        const timedOut = error.name === "AbortError" && error.cause?.name === "TimeoutError";
        if (!(timedOut || error instanceof Transient) || attempt >= site.spec.attempts) throw error;
        await sleep(site.spec.wait, undefined, { signal });
      }
    }
  } finally {
    live.exit();
  }
}

async function runItems(site, tenant, items, signal) {
  const failed = new AbortController();
  const stop = AbortSignal.any([signal, failed.signal]);
  let next = 0;
  let total = 0;
  const worker = async () => {
    while (next < items.length && !stop.aborted) {
      const item = items[next];
      next += 1;
      try {
        total += await one(site, tenant, item, stop);
      } catch (error) {
        failed.abort(error);
        throw error;
      }
    }
  };
  await Promise.all(Array.from({ length: Math.min(site.spec.bound, items.length) }, worker));
  return total;
}
// END pool

await measure("node-pool", process.argv[2] ?? "throughput", async (site, tenants, signal) => {
  const items = Array.from({ length: site.spec.items }, (_, item) => item);
  const totals = await Promise.all(tenants.map((tenant) => runItems(site, tenant, items, signal)));
  return totals.reduce((sum, value) => sum + value, 0);
});
