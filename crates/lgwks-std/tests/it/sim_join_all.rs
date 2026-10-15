//! Seeded simulation of `join_all`'s wake scheduling through its public API.
//!
//! `sim_task_public` drives the blocking pool with real threads, so its
//! interleavings belong to the OS. This family drives the other half of the
//! executor, the group scheduler, with nothing but futures on the calling
//! thread, so every interleaving is the seed's: one seed draws how many
//! children a group has, which tenant each belongs to, and what each does
//! (resolve at once, wake itself, wake itself twice per poll, wait on its
//! tenant's latch, or open that latch). The exact number of polls every child
//! receives is then predicted from the plan alone and compared, which is what
//! makes the properties checks rather than observations:
//!
//! - a wake polls only the child that woke (no re-poll of the pending set);
//! - two wakes before one poll cost one poll (the queue coalesces);
//! - a tenant's latch wakes only that tenant's waiters (a cross-tenant wake
//!   would add a poll the model does not have);
//! - outputs come back in input order whatever order children finish in.
//!
//! The source is the scheduler contract in `task/executor.rs`; the many-tenant
//! sizes are the workspace's concurrency tiers (100 to 100,000 children, 5,000
//! tenants provisioned in one group).

use std::cell::{Cell, RefCell};
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::task::{Context, Poll, Waker};

use lgwks_std::task::{block_on, join_all, join_all_boxed};

use crate::rng::Rng;
use crate::seeded_sweep::{
    SWEEP_SEEDS, assert_distinct_seeds_diverge, assert_same_seed_replays, fold_usize,
    initial_trace, next_index, seeded_stream,
};

/// The most children one seeded group holds in the sweep tests.
const MAX_CHILDREN: usize = 256;

/// The most self-wakes one seeded child spends before it resolves.
const MAX_WAKES: usize = 16;

/// The most tenants one seeded group is split across.
const MAX_TENANTS: usize = 8;

/// A boxed child, as `join_all_boxed` takes it.
type Boxed = Pin<Box<dyn Future<Output = usize>>>;

/// What one child does each time it is polled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Role {
    /// Resolves on its first poll.
    Ready,
    /// Wakes itself this many times, then resolves.
    SelfWaker(usize),
    /// Wakes itself twice per pending poll, this many times, then resolves.
    DoubleWaker(usize),
    /// Wakes itself this many times, then opens its tenant's latch and resolves.
    Opener(usize),
    /// Pends until its tenant's latch is open.
    Waiter,
}

impl Role {
    /// The self-wakes this role spends before it resolves.
    fn wakes(self) -> usize {
        match self {
            Self::SelfWaker(wakes) | Self::DoubleWaker(wakes) | Self::Opener(wakes) => wakes,
            Self::Ready | Self::Waiter => 0,
        }
    }
}

/// One child of a seeded group.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Plan {
    /// What the child does when polled.
    role: Role,
    /// The tenant whose latch it waits on or opens.
    tenant: usize,
}

/// One tenant's latch: closed until its opener resolves.
#[derive(Default)]
struct Latch {
    /// Whether the opener has resolved.
    open: Cell<bool>,
    /// Wakers of this tenant's waiters that pended while the latch was closed.
    parked: RefCell<Vec<Waker>>,
}

/// State every child of one run shares: the latches and what was observed.
struct World {
    /// One latch per tenant.
    latches: Vec<Latch>,
    /// Polls each child received, by position.
    polls: Vec<Cell<usize>>,
    /// The position of every poll, in the order the scheduler made them.
    order: RefCell<Vec<usize>>,
}

/// One seeded child: a future whose behaviour is its plan.
struct Child {
    /// Its position in the group.
    index: usize,
    /// What it does.
    plan: Plan,
    /// Self-wakes left before it resolves (or opens its latch).
    remaining: usize,
    /// The value it resolves to, unique to this seed and position.
    value: usize,
    /// The run's shared state.
    world: Rc<World>,
}

impl Child {
    /// The latch of this child's tenant. Every plan names a tenant the world
    /// built a latch for, so a missing latch is a fixture defect, seen as a
    /// child that never resolves rather than as a panic.
    fn latch(&self) -> Option<&Latch> {
        self.world.latches.get(self.plan.tenant)
    }

    /// Opens this child's latch and wakes every waiter parked on it.
    fn open_latch(&self) {
        let Some(latch) = self.latch() else {
            return;
        };
        latch.open.set(true);
        let parked: Vec<Waker> = latch.parked.borrow_mut().drain(..).collect();
        for waker in parked {
            waker.wake();
        }
    }
}

impl Future for Child {
    type Output = usize;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<usize> {
        let this = self.get_mut();
        this.world.order.borrow_mut().push(this.index);
        if let Some(count) = this.world.polls.get(this.index) {
            count.set(count.get().saturating_add(1));
        }
        if this.remaining > 0 {
            this.remaining = this.remaining.saturating_sub(1);
            cx.waker().wake_by_ref();
            if matches!(this.plan.role, Role::DoubleWaker(_)) {
                cx.waker().wake_by_ref();
            }
            return Poll::Pending;
        }
        match this.plan.role {
            Role::Ready | Role::SelfWaker(_) | Role::DoubleWaker(_) => Poll::Ready(this.value),
            Role::Opener(_) => {
                this.open_latch();
                Poll::Ready(this.value)
            }
            Role::Waiter => match this.latch() {
                Some(latch) if latch.open.get() => Poll::Ready(this.value),
                Some(latch) => {
                    latch.parked.borrow_mut().push(cx.waker().clone());
                    Poll::Pending
                }
                None => Poll::Pending,
            },
        }
    }
}

/// The value child `index` of the group seeded by `seed` resolves to: from the
/// seed and position alone, so a value that crossed to another child cannot
/// match.
fn expected(seed: u64, index: usize) -> usize {
    let mut stream = seeded_stream(seed);
    next_index(&mut stream).wrapping_add(index)
}

/// Every output a correct run of `children` children returns, in input order.
fn expected_outputs(seed: u64, children: usize) -> Vec<usize> {
    (0..children).map(|index| expected(seed, index)).collect()
}

/// Draws `children` plans over `tenants` tenants from `rng`, then gives every
/// tenant that has a child exactly one opener, so every waiter is released.
fn plan_sized(rng: &mut Rng, children: usize, tenants: usize, max_wakes: usize) -> Vec<Plan> {
    let tenants = tenants.max(1);
    let mut plans: Vec<Plan> = (0..children)
        .map(|_| {
            let tenant = rng.below(tenants);
            let role = match rng.below(4) {
                0 => Role::Ready,
                1 => Role::SelfWaker(rng.below(max_wakes)),
                2 => Role::DoubleWaker(rng.below(max_wakes)),
                _ => Role::Waiter,
            };
            Plan { role, tenant }
        })
        .collect();
    let mut members: Vec<Vec<usize>> = (0..tenants).map(|_| Vec::new()).collect();
    for (index, plan) in plans.iter().enumerate() {
        if let Some(list) = members.get_mut(plan.tenant) {
            list.push(index);
        }
    }
    for list in members {
        let Some(&chosen) = list.get(rng.below(list.len())) else {
            continue;
        };
        let wakes = rng.below(max_wakes);
        if let Some(plan) = plans.get_mut(chosen) {
            plan.role = Role::Opener(wakes);
        }
    }
    plans
}

/// The group `seed` draws for the sweep tests.
fn plan(seed: u64) -> Vec<Plan> {
    let mut rng = Rng::new(seed);
    let children = rng.below(MAX_CHILDREN).saturating_add(1);
    let tenants = rng.below(MAX_TENANTS).saturating_add(1);
    plan_sized(&mut rng, children, tenants, MAX_WAKES)
}

/// `waiters` waiters of tenant 0 and that tenant's one opener, with `wakes`
/// self-wakes, either ahead of them or behind them.
fn one_tenant(waiters: usize, wakes: usize, opener_first: bool) -> Vec<Plan> {
    let opener = Plan {
        role: Role::Opener(wakes),
        tenant: 0,
    };
    let waiting = (0..waiters).map(|_| Plan {
        role: Role::Waiter,
        tenant: 0,
    });
    if opener_first {
        std::iter::once(opener).chain(waiting).collect()
    } else {
        waiting.chain(std::iter::once(opener)).collect()
    }
}

/// How many tenants the plans name: one past the highest.
fn tenant_count(plans: &[Plan]) -> usize {
    plans
        .iter()
        .fold(0, |most, plan| most.max(plan.tenant.saturating_add(1)))
}

/// The polls each child must receive, predicted from the plan alone.
///
/// The first round polls every child once, in input order. A self-waking child
/// costs one poll per wake plus the resolving one; two wakes before a poll
/// coalesce into one. A waiter resolves on its first poll if its tenant's
/// opener already opened the latch earlier in that round (an opener with no
/// wakes, ahead of it), and on its second poll otherwise: exactly one wake,
/// from its own tenant's opener, re-polls it.
fn model_polls(plans: &[Plan]) -> Vec<usize> {
    let mut openers: Vec<Option<(usize, usize)>> = vec![None; tenant_count(plans)];
    for (index, plan) in plans.iter().enumerate() {
        if let (Role::Opener(wakes), Some(slot)) = (plan.role, openers.get_mut(plan.tenant)) {
            *slot = Some((index, wakes));
        }
    }
    plans
        .iter()
        .enumerate()
        .map(|(index, plan)| match plan.role {
            Role::Waiter => match openers.get(plan.tenant).copied().flatten() {
                Some((opener, 0)) if opener < index => 1,
                _ => 2,
            },
            role => role.wakes().saturating_add(1),
        })
        .collect()
}

/// What one run observed.
struct Outcome {
    /// Outputs in the order the group returned them.
    outputs: Vec<usize>,
    /// Polls each child received.
    polls: Vec<usize>,
    /// Every poll's position, in scheduling order.
    order: Vec<usize>,
}

/// A fresh world for `plans`, and its children.
fn build(seed: u64, plans: &[Plan]) -> (Rc<World>, Vec<Child>) {
    let world = Rc::new(World {
        latches: (0..tenant_count(plans)).map(|_| Latch::default()).collect(),
        polls: plans.iter().map(|_| Cell::new(0)).collect(),
        order: RefCell::new(Vec::new()),
    });
    let children = plans
        .iter()
        .enumerate()
        .map(|(index, &plan)| Child {
            index,
            plan,
            remaining: plan.role.wakes(),
            value: expected(seed, index),
            world: Rc::clone(&world),
        })
        .collect();
    (world, children)
}

/// Reads what a finished run observed out of its world.
fn observe(world: &World, outputs: Vec<usize>) -> Outcome {
    Outcome {
        outputs,
        polls: world.polls.iter().map(Cell::get).collect(),
        order: world.order.borrow().clone(),
    }
}

/// Runs `plans` as one boxed group on the calling thread.
fn run(seed: u64, plans: &[Plan]) -> Outcome {
    let (world, children) = build(seed, plans);
    let boxed: Vec<Boxed> = children
        .into_iter()
        .map(|child| -> Boxed { Box::pin(child) })
        .collect();
    let outputs = block_on(join_all_boxed(boxed));
    observe(&world, outputs)
}

/// Runs `plans` and asserts every property the model predicts.
fn assert_matches_model(seed: u64, plans: &[Plan]) -> Outcome {
    let outcome = run(seed, plans);
    assert_eq!(
        outcome.outputs,
        expected_outputs(seed, plans.len()),
        "seed {seed:#x}: outputs must come back in input order, each its own child's"
    );
    assert_eq!(
        outcome.polls,
        model_polls(plans),
        "seed {seed:#x}: every child must receive exactly the polls its plan predicts"
    );
    outcome
}

/// One seed's run as a trace hash: the scheduling order and the outputs.
fn trace(seed: u64) -> u64 {
    let outcome = run(seed, &plan(seed));
    let mut hash = initial_trace();
    for &position in outcome.order.iter().chain(&outcome.outputs) {
        fold_usize(&mut hash, position);
    }
    hash
}

#[test]
/// Every child receives exactly the polls the wake model predicts, and the
/// outputs come back in input order: no child is re-polled because a sibling
/// woke, and none is starved of the poll its own wake asked for.
fn sim_poll_counts_and_outputs_match_the_wake_model() {
    for seed in SWEEP_SEEDS {
        assert_matches_model(seed, &plan(seed));
    }
}

#[test]
/// The first round polls every child exactly once, in input order, and no
/// child is polled past its resolving poll.
fn sim_first_round_in_order_and_no_poll_after_resolution() {
    for seed in SWEEP_SEEDS {
        let plans = plan(seed);
        let outcome = assert_matches_model(seed, &plans);
        let first_round: Vec<usize> = outcome.order.iter().copied().take(plans.len()).collect();
        assert_eq!(
            first_round,
            (0..plans.len()).collect::<Vec<_>>(),
            "seed {seed:#x}: the opening round polls each child once, in order"
        );
        let model = model_polls(&plans);
        let mut seen = vec![0_usize; plans.len()];
        for &position in &outcome.order {
            if let Some(count) = seen.get_mut(position) {
                *count = count.saturating_add(1);
                assert!(
                    Some(&*count) <= model.get(position),
                    "seed {seed:#x}: child {position} polled past its resolving poll"
                );
            }
        }
    }
}

#[test]
/// Two wakes before one poll cost one poll: the group's queue holds a child at
/// most once.
fn sim_duplicate_wakes_before_a_poll_coalesce() {
    for seed in SWEEP_SEEDS {
        let mut rng = Rng::new(seed);
        let children = rng.below(MAX_CHILDREN).saturating_add(1);
        let plans: Vec<Plan> = (0..children)
            .map(|_| Plan {
                role: Role::DoubleWaker(rng.below(MAX_WAKES)),
                tenant: 0,
            })
            .collect();
        assert_matches_model(seed, &plans);
    }
}

#[test]
/// One tenant's latch wakes only that tenant's waiters: tenant 0 opens at the
/// front, and tenant 1's waiters behind it are released only by tenant 1's own
/// opener at the back.
fn sim_one_tenants_latch_never_polls_another_tenants_waiters() {
    for seed in SWEEP_SEEDS {
        let mut rng = Rng::new(seed);
        let waiters = rng.below(MAX_CHILDREN).saturating_add(1);
        let mut plans = vec![Plan {
            role: Role::Opener(0),
            tenant: 0,
        }];
        plans.extend([0, 1].into_iter().cycle().take(waiters).map(|tenant| Plan {
            role: Role::Waiter,
            tenant,
        }));
        plans.push(Plan {
            role: Role::Opener(rng.below(MAX_WAKES).saturating_add(1)),
            tenant: 1,
        });
        assert_matches_model(seed, &plans);
    }
}

#[test]
/// A waiter behind an opened latch resolves on its first poll; a waiter ahead
/// of its opener is polled exactly twice, the second time by its own opener;
/// an opener that wakes itself up to a thousand times does not re-poll them.
fn sim_waiters_cost_what_their_latch_order_predicts() {
    for seed in SWEEP_SEEDS {
        let mut rng = Rng::new(seed);
        let waiters = rng.below(MAX_CHILDREN).saturating_add(1);
        let behind = assert_matches_model(seed, &one_tenant(waiters, 0, true));
        assert!(
            behind.polls.iter().all(|&polls| polls == 1),
            "seed {seed:#x}"
        );
        let wakes = rng.below(1_000).saturating_add(1);
        let ahead = assert_matches_model(seed, &one_tenant(waiters, wakes, false));
        let waiter_polls: Vec<usize> = ahead.polls.iter().copied().take(waiters).collect();
        assert_eq!(waiter_polls, vec![2; waiters], "seed {seed:#x}");
    }
}

#[test]
/// The same seed replays the same scheduling order and outputs, and distinct
/// seeds drive distinct groups, so the sweep is reading its seed.
fn sim_the_same_seed_replays_and_distinct_seeds_diverge() {
    for seed in SWEEP_SEEDS {
        assert_same_seed_replays(trace, seed);
    }
    let [first, second, ..] = SWEEP_SEEDS;
    assert_distinct_seeds_diverge(trace, first, second);
}

#[test]
/// An empty group resolves at once, with nothing polled.
fn sim_an_empty_group_resolves_without_a_poll() {
    let outcome = run(0, &[]);
    assert!(
        outcome.outputs.is_empty() && outcome.order.is_empty(),
        "an empty group returns nothing and polls nothing"
    );
}

#[test]
/// `join_all` over unboxed children and `join_all_boxed` over boxed ones agree
/// on every output, every poll count and the whole scheduling order.
fn sim_join_all_and_join_all_boxed_agree() {
    for seed in SWEEP_SEEDS {
        let plans = plan(seed);
        let boxed = run(seed, &plans);
        let (world, children) = build(seed, &plans);
        let outputs = block_on(join_all(children));
        let unboxed = observe(&world, outputs);
        assert_eq!(unboxed.outputs, boxed.outputs, "seed {seed:#x}");
        assert_eq!(unboxed.polls, boxed.polls, "seed {seed:#x}");
        assert_eq!(unboxed.order, boxed.order, "seed {seed:#x}");
    }
}

#[test]
/// A group of per-tenant groups keeps both orders: tenants in input order, and
/// each tenant's children in theirs, with every child polled at least once.
fn sim_nested_groups_keep_each_tenants_order() {
    for seed in SWEEP_SEEDS {
        let plans = plan(seed);
        let tenants = tenant_count(&plans);
        let (world, children) = build(seed, &plans);
        let mut groups: Vec<Vec<Boxed>> = (0..tenants).map(|_| Vec::new()).collect();
        let mut expected_groups: Vec<Vec<usize>> = (0..tenants).map(|_| Vec::new()).collect();
        for child in children {
            let tenant = child.plan.tenant;
            if let (Some(group), Some(values)) =
                (groups.get_mut(tenant), expected_groups.get_mut(tenant))
            {
                values.push(child.value);
                group.push(Box::pin(child));
            }
        }
        let outputs = block_on(join_all(groups.into_iter().map(join_all_boxed)));
        assert_eq!(outputs, expected_groups, "seed {seed:#x}");
        let observed = observe(&world, Vec::new());
        assert!(
            observed.polls.iter().all(|&polls| polls >= 1),
            "seed {seed:#x}: every child was polled"
        );
    }
}

/// One seeded group of exactly `children` children across `tenants` tenants,
/// checked against the model under every sweep seed.
fn assert_tier(children: usize, tenants: usize, max_wakes: usize) {
    for seed in SWEEP_SEEDS {
        let mut rng = Rng::new(seed);
        assert_matches_model(seed, &plan_sized(&mut rng, children, tenants, max_wakes));
    }
}

#[test]
/// Concurrency tiers 100 and 1,000: that many children in flight in one group.
fn sim_tiers_100_and_1000_match_the_model() {
    assert_tier(100, MAX_TENANTS, MAX_WAKES);
    assert_tier(1_000, MAX_TENANTS, MAX_WAKES);
}

#[test]
/// Concurrency tier 10,000.
fn sim_tier_10000_matches_the_model() {
    assert_tier(10_000, MAX_TENANTS, 4);
}

#[test]
/// Concurrency tier 100,000: the scheduler stays linear in the wakes, not in
/// the pending set, or this tier would not finish.
fn sim_tier_100000_matches_the_model() {
    assert_tier(100_000, MAX_TENANTS, 2);
}

#[test]
/// 5,000 tenants provisioned in one group, each with its own latch, stay
/// isolated: every waiter receives exactly the polls its own tenant's opener
/// predicts.
fn sim_5000_tenants_in_one_group_stay_isolated() {
    assert_tier(10_000, 5_000, 2);
}
