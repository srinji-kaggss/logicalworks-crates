//! How an admitted observation is named for a dispatch digest: the InputIdentity trait, its fixed-width and string impls, and EventId for two equal payloads that are two events.

use lgwks_std::hash::Hasher;

/// How an admitted observation is named for the purpose of a dispatch digest.
///
/// A content identity, not an event number: the same value of the same type
/// writes the same bytes (so a restart can retire work that already landed),
/// and a different value writes different ones (so a restart cannot read new
/// work as already applied). Two *events* with equal content must still be
/// distinguishable — content equality is not event identity — so a caller that
/// needs that puts the event id into the value and into this trait. See
/// [`EventId`].
///
/// Every integer writes `to_le_bytes` of a fixed width, so a 32-bit and a
/// 64-bit target derive the same identity (issue #101).
///
/// # The schema id is the durable name of the type (issue #118)
///
/// [`SCHEMA_ID`](Self::SCHEMA_ID) is what binds identity bytes to a type, and
/// it is a *declared* versioned string, never `std::any::type_name`. Rust
/// documents `type_name` as diagnostic: its format is unspecified and it may
/// change between compiler versions, so a module rename or a toolchain upgrade
/// would silently give every admitted event a new identity and resurrect
/// settled work as fresh. A declared id survives all of that; a *changed* id
/// is an explicit schema migration whose identities differ by construction —
/// recovered work under the old id is refused as superseded rather than
/// silently folded. `type_name` remains in use only where a human reads it
/// (diagnostics), never inside a digest.
pub trait InputIdentity {
    /// The versioned schema identity of `Self`, declared by the type's author.
    ///
    /// Convention: `lgwks.bot.schema.v1.<kind>`, lowercased, stable across
    /// builds and compilers. Bump the version when the identity *bytes* change
    /// meaning — that bump is the migration boundary, and identities across it
    /// are distinct by construction.
    const SCHEMA_ID: &'static [u8];

    /// Write this input's canonical identity bytes into `hasher`.
    ///
    /// Variable-length parts go through [`Hasher::write_framed`] so the
    /// stream's field boundaries are part of the digest.
    fn write_identity(&self, hasher: &mut Hasher);

    /// Whether these identity bytes name an *event*, rather than only content.
    ///
    /// The two readings of a returning identity are different questions and
    /// only one of them is right for a given type (issue #129).
    ///
    /// - `true` — the bytes name an event, so equal bytes mean the same event
    ///   has come back. That is a redelivery of work that already landed, and
    ///   it retires however many other episodes came between. [`EventId`] is
    ///   this: its identity carries the caller's event id.
    /// - `false` (the default) — the bytes name only content, so a state watch
    ///   returning to a previous value is a *new* episode of work rather than a
    ///   redelivery. It runs again. Only the value the last applied episode
    ///   carried retires, which is what keeps a restart from double-firing on
    ///   an unchanged source while a `0 → 1 → 0` watch still fires three times.
    ///
    /// Defaulting to `false` is deliberate: a type that has not claimed event
    /// identity gets the state-transition semantics the watch contract
    /// documents, and opting into `true` is a statement about the type.
    fn names_an_event(&self) -> bool {
        false
    }
}

/// Implement [`InputIdentity`] for a fixed-width integer via `to_le_bytes`.
macro_rules! input_identity_int {
    ($($t:ty => $name:literal),* $(,)?) => {$(
        impl InputIdentity for $t {
            const SCHEMA_ID: &'static [u8] =
                concat!("lgwks.bot.schema.v1.int-", $name).as_bytes();
            fn write_identity(&self, hasher: &mut Hasher) {
                hasher.update(&self.to_le_bytes());
            }
        }
    )*};
}

input_identity_int!(
    u8 => "u8",
    u16 => "u16",
    u32 => "u32",
    u64 => "u64",
    i8 => "i8",
    i16 => "i16",
    i32 => "i32",
    i64 => "i64",
);

impl InputIdentity for bool {
    const SCHEMA_ID: &'static [u8] = b"lgwks.bot.schema.v1.bool";

    fn write_identity(&self, hasher: &mut Hasher) {
        hasher.update(&[u8::from(*self)]);
    }
}

impl InputIdentity for String {
    const SCHEMA_ID: &'static [u8] = b"lgwks.bot.schema.v1.string";

    fn write_identity(&self, hasher: &mut Hasher) {
        hasher.write_framed(self.as_bytes());
    }
}

impl InputIdentity for &str {
    const SCHEMA_ID: &'static [u8] = b"lgwks.bot.schema.v1.string";

    fn write_identity(&self, hasher: &mut Hasher) {
        hasher.write_framed(self.as_bytes());
    }
}

/// A value tagged with an event id, so two equal payloads stay two events.
///
/// The id is written first and is part of the identity: `EventId { id: 1,
/// value: 5 }` and `EventId { id: 2, value: 5 }` are different admitted
/// inputs even though the payloads compare equal. `PartialEq` includes the id
/// for the same reason — the change filter is content equality, and two
/// distinct events that compare equal would be collapsed into one revision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct EventId<T> {
    /// The caller's event identity. Distinct per event, including events that
    /// carry equal payloads.
    id: u64,
    /// The payload.
    value: T,
}

impl<T> EventId<T> {
    /// Tag `value` as event `id`.
    #[must_use]
    pub const fn new(id: u64, value: T) -> Self {
        Self { id, value }
    }

    /// The caller's event identity.
    #[must_use]
    pub const fn id(&self) -> u64 {
        self.id
    }

    /// The payload.
    #[must_use]
    pub const fn value(&self) -> &T {
        &self.value
    }
}

impl<T: InputIdentity> InputIdentity for EventId<T> {
    const SCHEMA_ID: &'static [u8] = b"lgwks.bot.schema.v1.event-id";

    fn write_identity(&self, hasher: &mut Hasher) {
        // The payload's own schema travels inside the stream: two wrappers
        // over different payload types stay distinct identities even when
        // their ids and payload bytes coincide, which the erased
        // `type_name`-based scheme could only tell apart unreliably.
        hasher.write_framed(T::SCHEMA_ID);
        hasher.update(&self.id.to_le_bytes());
        self.value.write_identity(hasher);
    }

    /// The id is written into the identity, so equal identity bytes really do
    /// mean the same event. A returning one is a redelivery and retires.
    fn names_an_event(&self) -> bool {
        true
    }
}
