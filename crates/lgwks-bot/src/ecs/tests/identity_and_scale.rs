//! `ecs` tests: output identities, known-answer vectors and settlement at scale.

use super::*;

// ── admitted-input identity framing ─────────────────────────────────────

/// Two output types whose `type_name`s share a prefix, the pair the
/// identity scheme must keep apart (issue #118).
///
/// `PrefixProbeTail`'s type name is `PrefixProbe`'s name plus `b"Tail"`,
/// and `PrefixProbe` writes that same suffix at the head of its identity
/// bytes, so an unframed `name || bytes` stream would collide byte for
/// byte. Under the declared-schema scheme they are distinct because their
/// authors declared distinct schema ids — the name games cannot reach the
/// digest any more.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PrefixProbe;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PrefixProbeTail;

/// The byte difference between the two type names above.
const PREFIX_PROBE_SUFFIX: &[u8] = b"Tail";

impl InputIdentity for PrefixProbe {
    const SCHEMA_ID: &'static [u8] = b"lgwks.bot.schema.v1.test-prefix-probe";

    fn write_identity(&self, hasher: &mut Hasher) {
        hasher.update(PREFIX_PROBE_SUFFIX);
        hasher.update(b"payload");
    }
}

impl InputIdentity for PrefixProbeTail {
    const SCHEMA_ID: &'static [u8] = b"lgwks.bot.schema.v1.test-prefix-probe-tail";

    fn write_identity(&self, hasher: &mut Hasher) {
        hasher.update(b"payload");
    }
}

/// A second schema of a type whose identity bytes are byte-for-byte
/// identical to [`PrefixProbe`]'s — the control that proves the schema
/// id, not the bytes, separates the two.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PrefixProbeNextSchema;

impl InputIdentity for PrefixProbeNextSchema {
    const SCHEMA_ID: &'static [u8] = b"lgwks.bot.schema.v2.test-prefix-probe";

    fn write_identity(&self, hasher: &mut Hasher) {
        hasher.update(PREFIX_PROBE_SUFFIX);
        hasher.update(b"payload");
    }
}

/// The source type for the identity known-answer vectors.
///
/// [`identify_output`] reads only a source's output type, so these tests name
/// a source without building or ticking a bot over it. One generic type
/// stands for every output they derive an identity for, and a poll — which
/// none of them makes — is refused rather than answered.
struct Unpolled<T>(PhantomData<fn() -> T>);

impl<T> Observe for Unpolled<T> {
    type Output = T;

    fn required_caps(&self) -> &[Cap] {
        &[]
    }

    async fn poll(&self, _call: (Auth, ())) -> Result<T, BotError> {
        domain_refusal("test::unpolled", "identity fixtures are never polled")
    }

    fn domain_id(&self) -> &str {
        "test::unpolled"
    }
}

/// A payload whose identity bytes are identical to `u64`'s — the control
/// that proves the inner schema id, not the bytes, keeps two wrapper
/// types apart.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct AltTick(u64);

impl InputIdentity for AltTick {
    const SCHEMA_ID: &'static [u8] = b"lgwks.bot.schema.v1.test-alt-tick";

    fn write_identity(&self, hasher: &mut Hasher) {
        hasher.update(&self.0.to_le_bytes());
    }
}

#[test]
fn two_output_types_sharing_a_name_prefix_hash_two_identities() {
    let plain = identify_output::<Unpolled<PrefixProbe>>(&PrefixProbe);
    let tail = identify_output::<Unpolled<PrefixProbeTail>>(&PrefixProbeTail);
    assert_ne!(
        plain.identity, tail.identity,
        "the declared schema ids must separate the pair their name and \
         byte streams would collide"
    );
}

#[test]
fn a_schema_bump_moves_the_identity_of_identical_bytes() {
    // The declared migration boundary: same identity bytes under a
    // different schema id are different admitted inputs. A recovered
    // journal from the old schema therefore derives a different dispatch
    // digest and is refused as superseded rather than folded.
    let current = identify_output::<Unpolled<PrefixProbe>>(&PrefixProbe);
    let next = identify_output::<Unpolled<PrefixProbeNextSchema>>(&PrefixProbeNextSchema);
    assert_ne!(current.identity, next.identity);
}

#[test]
fn the_same_output_hashes_one_identity_every_time() {
    let first = identify_output::<Script>(&7_u16);
    let second = identify_output::<Script>(&7_u16);
    assert_eq!(first.identity, second.identity);
    assert_eq!(first.event, second.event);
}

#[test]
fn a_downcast_miss_still_names_the_binding_it_refused() {
    let hit = identify_output::<Script>(&7_u16);
    let miss = identify_output::<Script>(&1_000_u32);
    assert_ne!(
        hit.identity, miss.identity,
        "a missed downcast must not read as an admission of the value"
    );
}

#[test]
fn the_same_event_twice_and_two_events_hash_as_documented() {
    let redelivery = identify_output::<Unpolled<EventId<u64>>>(&EventId::new(5_u64, 7_u64));
    let again = identify_output::<Unpolled<EventId<u64>>>(&EventId::new(5_u64, 7_u64));
    assert_eq!(
        redelivery.identity, again.identity,
        "the same event id over the same payload is one event"
    );
    assert!(redelivery.event, "EventId names an event");

    let other_id = identify_output::<Unpolled<EventId<u64>>>(&EventId::new(6_u64, 7_u64));
    assert_ne!(redelivery.identity, other_id.identity);

    // The payload's schema rides inside the wrapper's stream, so equal
    // ids over identical written bytes from different payload types stay
    // distinct.
    let other_payload =
        identify_output::<Unpolled<EventId<AltTick>>>(&EventId::new(5_u64, AltTick(7_u64)));
    assert_ne!(redelivery.identity, other_payload.identity);
}

/// Known-answer vectors for the `v2` identity scheme (issue #118).
///
/// Each vector pins the exact 16-byte identity a stream must produce from
/// its declared parts: domain, schema id, identity bytes. A toolchain,
/// module path or crate rename cannot move these values — the stream
/// contains none of them — so any change here is a deliberate scheme
/// change, and the vector is what refuses it.
#[test]
fn identity_known_answer_vectors_hold() {
    let hex = |identity: [u8; 16]| {
        identity
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    };

    let u16_seven = hex(identify_output::<Script>(&7_u16).identity);
    assert_eq!(
        u16_seven, "878e8add50e816abdbb0901de9ed7788",
        "the u16 vector moved: the v2 identity scheme changed"
    );

    let str_value = identify_output::<Unpolled<String>>(&String::from("settle")).identity;
    assert_eq!(
        hex(str_value),
        "174d75182e8e31c49374da2f22e0b353",
        "the string vector moved: the v2 identity scheme changed"
    );

    let event = identify_output::<Unpolled<EventId<u64>>>(&EventId::new(9_u64, true)).identity;
    assert_eq!(
        hex(event),
        "f4d5d8a77b9fed69dd1474cf0620348a",
        "the event-id vector moved: the v2 identity scheme changed"
    );

    // A refused binding has its own vector: the miss names the schema it
    // refused, and the value it refused is deliberately absent.
    let miss = identify_output::<Script>(&1_000_u32).identity;
    assert_eq!(
        hex(miss),
        "f2e18c137ba6e62e78f672c22c02bc66",
        "the refused-binding vector moved: the v2 identity scheme changed"
    );
}

/// The applied-key index answers the same questions the vector did, at a
/// history size where scanning before every push was quadratic.
///
/// This drives the shipped path: [`Effects::note_applied`] is what the live
/// settle and the recovered seeding both call, and [`Effects::applied_in`]
/// is what the walk asks before it dispatches. Each key's digest is the one
/// `applied_in` recomputes for its coordinates, which is the discriminating
/// part — a wrong index answers false for a key it holds. The old
/// `Vec<EffectKey>` would need Θ(n²) comparisons to build this set; the
/// indexed form builds and queries it in one pass.
#[test]
fn applied_membership_answers_at_scale_where_a_scan_would_be_quadratic() -> TestResult {
    /// Distinct actions, one per obligation. Large enough that the removed
    /// scan-before-push is visibly quadratic, small enough for a suite.
    const COUNT: u64 = 50_000;

    /// A distinct 16-byte action id for `index`.
    fn action_for(index: u64) -> Result<ActionId, Box<dyn std::error::Error>> {
        Ok(ActionId::from_hex(&format!(
            "{:032x}",
            index.wrapping_add(1)
        ))?)
    }
    /// The coordinates `applied_in` would ask about for `index`.
    fn coordinates(index: u64) -> Result<(usize, usize), Box<dyn std::error::Error>> {
        let chain = index.checked_rem(16).ok_or("sixteen chains is not zero")?;
        let entry = index.checked_div(16).ok_or("sixteen chains is not zero")?;
        Ok((usize::try_from(chain)?, usize::try_from(entry)?))
    }

    let scope = test_effects()?;
    let mut effects = Effects::new(scope, JournalPosition::genesis());
    let identity = effects.identity();
    let flow = identity.flow();
    let run = identity.run();
    let environment = identity.environment();
    let epoch = EnvironmentEpoch::from_decimal("1")?;
    let attempt = AttemptId::from_decimal("1")?;
    let input = [0u8; 16];

    for index in 0..COUNT {
        let (chain, entry) = coordinates(index)?;
        let key = crate::effect::EffectIdentity::new(run, environment, flow).key(
            action_for(index)?,
            attempt,
            derive_action_digest(flow, chain, entry, &input),
            epoch,
        );
        effects.note_applied(key);
    }

    // A duplicate settlement must not grow either index.
    let (chain, entry) = coordinates(0)?;
    effects.note_applied(
        crate::effect::EffectIdentity::new(run, environment, flow).key(
            action_for(0)?,
            attempt,
            derive_action_digest(flow, chain, entry, &input),
            epoch,
        ),
    );
    assert_eq!(
        effects.applied_events.len(),
        usize::try_from(COUNT)?,
        "a duplicate settlement changed the event-identity index"
    );
    assert_eq!(
        effects.applied_latest.len(),
        usize::try_from(COUNT)?,
        "a duplicate settlement changed the latest-key index"
    );

    for index in 0..COUNT {
        let (chain, entry) = coordinates(index)?;
        let action = action_for(index)?;
        assert!(
            effects.applied_in(action, chain, entry, input, true),
            "the event identity for obligation {index} is absent from the index"
        );
        assert!(
            effects.applied_in(action, chain, entry, input, false),
            "the contract identity for obligation {index} is absent from the index"
        );
    }

    // An action the run never applied is not held, in either identity.
    let absent = action_for(COUNT)?;
    assert!(
        !effects.applied_in(absent, 0, 0, input, true)
            && !effects.applied_in(absent, 0, 0, input, false),
        "an unapplied obligation must not resolve as done"
    );
    Ok(())
}

/// A durable journal that grades itself `ProcessCrash` but refuses to
/// reserve settlement capacity, counting the appends it was asked for.
///
/// The shipped [`FileJournal`] reserves room
/// against its own event ceiling; this stands in for the moment that
/// ceiling is reached, which a test cannot reach without a hundred thousand
/// real flushes.
struct NoSettlementRoom {
    /// The record the appends would have gone to.
    inner: MemoryJournal,
    /// How many appends were asked for, whether or not one happened.
    appends: Rc<Cell<usize>>,
}

impl EffectJournal for NoSettlementRoom {
    fn durability(&self) -> DurabilityPromise {
        DurabilityPromise::ProcessCrash
    }

    fn tail(&self) -> JournalPosition {
        self.inner.tail()
    }

    fn committed(&self) -> Result<Vec<EffectEvent>, JournalError> {
        EffectJournal::committed(&self.inner)
    }

    fn compare_and_append(
        &mut self,
        expected_tail: JournalPosition,
        event: &EffectEvent,
    ) -> Result<DurableAck, JournalError> {
        self.appends.set(self.appends.get().saturating_add(1));
        self.inner.compare_and_append(expected_tail, event)
    }

    fn reserve_handoff_capacity(&self, rungs: u64) -> Result<(), JournalError> {
        Err(JournalError::CapacityExceeded {
            resource: crate::journal::JournalLimitKind::Events,
            limit: 0,
            requested: rungs,
        })
    }
}

/// An external handoff is refused before any rung when the settlement
/// record does not fit, so no attempt is ever left admitted and unable to
/// settle.
///
/// Acceptance for #122 item 2 / #156: the reservation happens before the
/// first write, so a journal that cannot hold the settlement refuses the
/// handoff and the store stays untouched — rather than admitting the intent
/// and the preparation and then being unable to persist the outcome.
#[test]
fn an_external_handoff_reserves_settlement_capacity_before_any_rung() -> TestResult {
    let appends = Rc::new(Cell::new(0));
    let journal = NoSettlementRoom {
        inner: MemoryJournal::new(),
        appends: Rc::clone(&appends),
    };
    let scope = test_effects_with(Box::new(journal))?;
    let mut effects = Effects::new(scope, JournalPosition::genesis());
    let key = crate::effect::EffectIdentity::new(
        RunId::from_hex(TEST_RUN)?,
        EnvironmentId::from_hex(TEST_ENV)?,
        FlowRevision::from_tagged("blake3_256", TEST_FLOW)?,
    )
    .key(
        ActionId::from_hex("1112131415161718191a1b1c1d1e1f20")?,
        AttemptId::from_decimal("1")?,
        ActionDigest::from_tagged(
            "blake3_256",
            "f0f1f2f3f4f5f6f7f8f9fafbfcfdfeffe0e1e2e3e4e5e6e7e8e9eaebecedeeef",
        )?,
        EnvironmentEpoch::from_decimal("1")?,
    );

    match lgwks_std::task::block_on(effects.prepare(key, EffectLifetime::External)) {
        Err(DispatchError::Journal(JournalError::CapacityExceeded { .. })) => {}
        Err(other) => {
            return Err(format!("expected a capacity refusal, got {other:?}").into());
        }
        Ok(_) => {
            return Err("a handoff with no settlement room must be refused".into());
        }
    }
    assert_eq!(
        appends.get(),
        0,
        "the handoff must be refused before the first rung is written"
    );
    Ok(())
}
