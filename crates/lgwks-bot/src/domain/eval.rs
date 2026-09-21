//! `eval` owns shipped evaluators — composable conditions for the Evaluate verb.

use crate::error::BotError;
use crate::verb::Evaluate;

/// True when a value has changed since last check. Requires the observed type
/// to implement `PartialEq + Clone`.
pub struct Changed<T: Clone + PartialEq> {
    /// The value seen on the previous check, or `None` before the first one.
    /// Interior mutability because `Evaluate::check` takes `&self`: a condition
    /// keeps state between ticks without the bot holding it mutably. `RefCell`
    /// rather than a lock — a bot is driven on one thread and a condition is
    /// never shared across threads.
    last: std::cell::RefCell<Option<T>>,
}

impl<T: Clone + PartialEq> Changed<T> {
    /// Create a new change detector.
    pub fn new() -> Self {
        Self {
            last: std::cell::RefCell::new(None),
        }
    }
}

impl<T: Clone + PartialEq> Default for Changed<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: Clone + PartialEq + 'static> Evaluate<T> for Changed<T> {
    fn check(&self, value: &T) -> Result<bool, BotError> {
        let mut last = self.last.borrow_mut();
        let changed = match last.as_ref() {
            Some(prev) => prev != value,
            None => true,
        };
        *last = Some(value.clone());
        Ok(changed)
    }

    fn condition_id(&self) -> &str {
        "changed"
    }
}

/// True when a numeric value crosses below a threshold.
pub struct Below<T> {
    /// The comparison bound. Strictly below: a value equal to the threshold
    /// does not fire.
    threshold: T,
}

impl<T: PartialOrd + 'static> Below<T> {
    /// Create a below-threshold evaluator.
    pub fn new(threshold: T) -> Self {
        Self { threshold }
    }
}

impl<T: PartialOrd + 'static> Evaluate<T> for Below<T> {
    fn check(&self, value: &T) -> Result<bool, BotError> {
        Ok(value < &self.threshold)
    }

    fn condition_id(&self) -> &str {
        "threshold::below"
    }
}

/// True when a numeric value crosses above a threshold.
pub struct Above<T> {
    /// The comparison bound. Strictly above: a value equal to the threshold
    /// does not fire.
    threshold: T,
}

impl<T: PartialOrd + 'static> Above<T> {
    /// Create an above-threshold evaluator.
    pub fn new(threshold: T) -> Self {
        Self { threshold }
    }
}

impl<T: PartialOrd + 'static> Evaluate<T> for Above<T> {
    fn check(&self, value: &T) -> Result<bool, BotError> {
        Ok(value > &self.threshold)
    }

    fn condition_id(&self) -> &str {
        "threshold::above"
    }
}

/// True when a string field contains a pattern.
pub struct Contains {
    /// The substring searched for, literally — this is not a pattern language,
    /// so a value that looks like a regex is matched as its own bytes.
    pattern: String,
}

impl Contains {
    /// Create a contains evaluator.
    #[must_use]
    pub fn new(pattern: impl Into<String>) -> Self {
        Self {
            pattern: pattern.into(),
        }
    }
}

impl Evaluate<String> for Contains {
    fn check(&self, value: &String) -> Result<bool, BotError> {
        Ok(value.contains(&self.pattern))
    }

    fn condition_id(&self) -> &str {
        "contains"
    }
}

impl Evaluate<super::chat::ChatMessage> for Contains {
    fn check(&self, value: &super::chat::ChatMessage) -> Result<bool, BotError> {
        Ok(value.text.contains(&self.pattern))
    }

    fn condition_id(&self) -> &str {
        "contains"
    }
}

/// Convenience: create a `Contains` evaluator.
#[must_use]
pub fn contains(pattern: impl Into<String>) -> Contains {
    Contains::new(pattern)
}

/// Convenience: create a `Changed` evaluator.
#[must_use]
pub fn changed<T: Clone + PartialEq>() -> Changed<T> {
    Changed::new()
}

/// Convenience: create a `Below` evaluator.
#[must_use]
pub fn below<T: PartialOrd + 'static>(threshold: T) -> Below<T> {
    Below::new(threshold)
}

/// Convenience: create an `Above` evaluator.
#[must_use]
pub fn above<T: PartialOrd + 'static>(threshold: T) -> Above<T> {
    Above::new(threshold)
}

/// True when all inner conditions pass.
pub struct All<T> {
    /// The inner conditions. Boxed and dynamic because `All` composes
    /// heterogeneous evaluators over the same `T`; a condition that errors is
    /// propagated rather than treated as a `false`, so a structural failure
    /// never reads as "the condition did not hold".
    conditions: Vec<Box<dyn Evaluate<T>>>,
}

impl<T: 'static> All<T> {
    /// Create an all-of combinator.
    #[must_use]
    pub fn new(conditions: Vec<Box<dyn Evaluate<T>>>) -> Self {
        Self { conditions }
    }
}

impl<T: 'static> Evaluate<T> for All<T> {
    fn check(&self, value: &T) -> Result<bool, BotError> {
        for condition in &self.conditions {
            if !condition.check(value)? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn condition_id(&self) -> &str {
        "all"
    }
}

/// True when any inner condition passes.
pub struct Any<T> {
    /// The inner conditions, checked in order and short-circuited on the first
    /// `true`. A condition that errors is propagated rather than treated as a
    /// `false`, so a structural failure never reads as "nothing matched".
    conditions: Vec<Box<dyn Evaluate<T>>>,
}

impl<T: 'static> Any<T> {
    /// Create an any-of combinator.
    #[must_use]
    pub fn new(conditions: Vec<Box<dyn Evaluate<T>>>) -> Self {
        Self { conditions }
    }
}

impl<T: 'static> Evaluate<T> for Any<T> {
    fn check(&self, value: &T) -> Result<bool, BotError> {
        for condition in &self.conditions {
            if condition.check(value)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn condition_id(&self) -> &str {
        "any"
    }
}
