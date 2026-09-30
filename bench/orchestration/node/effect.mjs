// Effect-TS, the closest declarative sibling of `script!`: typed errors,
// structured concurrency, `Effect.retry` with a `Schedule`, `Effect.timeout`,
// interruption that reaches every fiber.
//
//   node effect.mjs <scenario>   (needs effect, pinned in package-lock.json)

import { Effect, Schedule } from "effect";
import { Transient, measure } from "./site_model.mjs";

// BEGIN effect
const attempt = (site, key, item) =>
  Effect.suspend(() => Effect.sleep(site.begin(item))).pipe(
    Effect.flatMap(() => Effect.try({ try: () => site.settle(key, item), catch: (error) => error })),
  );

const one = (site, tenant, item) =>
  Effect.acquireUseRelease(
    Effect.sync(() => site.enter()),
    (live) =>
      attempt(site, `${tenant}/${item}`, item).pipe(
        Effect.timeoutFail({ duration: site.spec.deadline, onTimeout: () => new Transient("timed out") }),
        Effect.retry({
          times: site.spec.attempts - 1,
          schedule: Schedule.spaced(site.spec.wait),
          while: (error) => error instanceof Transient,
        }),
        Effect.tap(() => Effect.sync(() => live.finish())),
      ),
    (live) => Effect.sync(() => live.exit()),
  );

const runItems = (site, tenant, items) =>
  Effect.forEach(items, (item) => one(site, tenant, item), { concurrency: site.spec.bound }).pipe(
    Effect.map((values) => values.reduce((sum, value) => sum + value, 0)),
  );
// END effect

await measure("node-effect", process.argv[2] ?? "throughput", (site, tenants, signal) => {
  const items = Array.from({ length: site.spec.items }, (_, item) => item);
  const all = Effect.forEach(tenants, (tenant) => runItems(site, tenant, items), { concurrency: "unbounded" });
  return Effect.runPromise(
    all.pipe(Effect.map((totals) => totals.reduce((sum, value) => sum + value, 0))),
    { signal },
  );
});
