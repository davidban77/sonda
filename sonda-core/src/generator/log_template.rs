//! Template-based log generator — produces structured log events from message
//! templates with field pools.
//!
//! All selection (template, placeholder values, severity) is deterministic:
//! given the same `seed` and `tick`, `generate()` always produces an identical
//! `LogEvent`. No mutable state is required, satisfying the `&self` contract of
//! [`LogGenerator`].

use std::collections::{BTreeMap, HashMap};

use crate::model::log::{LogEvent, Severity};

use super::LogGenerator;

/// A single template entry defining a message pattern and the value pools for
/// each placeholder it contains.
pub(crate) struct TemplateEntry {
    /// The message template string. Placeholders use `{name}` syntax,
    /// e.g. `"Request from {ip} to {endpoint}"`.
    pub message: String,
    /// Maps each placeholder name to its pool of possible values.
    /// e.g. `{"ip": ["10.0.0.1", "10.0.0.2"], "endpoint": ["/api", "/health"]}`.
    pub field_pools: HashMap<String, Vec<String>>,
}

/// A log generator that produces events by selecting from message templates and
/// resolving placeholders from configurable value pools.
///
/// Templates are selected round-robin by tick index. Placeholder values and
/// severity are selected deterministically using a hash of `(seed, tick, name)`.
pub struct LogTemplateGenerator {
    templates: Vec<TemplateEntry>,
    severity_weights: Vec<(Severity, f64)>,
    seed: u64,
}

impl LogTemplateGenerator {
    /// Construct a new `LogTemplateGenerator`.
    ///
    /// # Parameters
    /// - `templates` — the set of template entries to draw from.
    /// - `severity_weights` — ordered list of `(severity, weight)` pairs. Weights
    ///   are normalized internally so they need not sum to 1.0. If empty, defaults
    ///   to `Info` with weight 1.0.
    /// - `seed` — determinism seed. Different seeds produce independent sequences.
    pub(crate) fn new(
        templates: Vec<TemplateEntry>,
        severity_weights: Vec<(Severity, f64)>,
        seed: u64,
    ) -> Self {
        let severity_weights = if severity_weights.is_empty() {
            vec![(Severity::Info, 1.0)]
        } else {
            severity_weights
        };
        Self {
            templates,
            severity_weights,
            seed,
        }
    }

    /// Mix a `u64` through a SplitMix64 finalizer.
    ///
    /// Stateless hash: same input always produces the same output.
    fn mix(mut z: u64) -> u64 {
        z = z.wrapping_add(0x9e37_79b9_7f4a_7c15);
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// Produce a deterministic `u64` hash from the seed, tick, and a string discriminant.
    ///
    /// The discriminant is hashed character-by-character to avoid allocations.
    fn hash_for(seed: u64, tick: u64, discriminant: &str) -> u64 {
        // Fold discriminant bytes into the hash to make each field independent.
        let mut h = Self::mix(seed ^ tick);
        for b in discriminant.bytes() {
            h = Self::mix(h ^ (b as u64));
        }
        h
    }

    /// Select a severity level deterministically based on weights.
    ///
    /// Uses a weighted selection: the cumulative weight thresholds are computed
    /// and the hash value (normalized to [0.0, 1.0)) picks the bucket.
    fn select_severity(&self, tick: u64) -> Severity {
        let total: f64 = self.severity_weights.iter().map(|(_, w)| w).sum();
        let hash = Self::hash_for(self.seed, tick, "severity");
        // Map hash to [0.0, 1.0)
        let unit = (hash as f64) / (u64::MAX as f64);
        let target = unit * total;

        let mut cumulative = 0.0;
        for (severity, weight) in &self.severity_weights {
            cumulative += weight;
            if target < cumulative {
                return *severity;
            }
        }
        // Fallback to last severity (handles floating-point edge cases)
        self.severity_weights
            .last()
            .map(|(s, _)| *s)
            .unwrap_or(Severity::Info)
    }

    /// Select a value from a pool deterministically.
    ///
    /// Uses the hash of `(seed, tick, field_name)` to pick an index.
    fn select_from_pool<'a>(seed: u64, tick: u64, field_name: &str, pool: &'a [String]) -> &'a str {
        if pool.is_empty() {
            return "";
        }
        let hash = Self::hash_for(seed, tick, field_name);
        let idx = (hash as usize) % pool.len();
        &pool[idx]
    }

    /// Resolve all `{placeholder}` occurrences in a template message.
    ///
    /// Returns the resolved message string and the fields map (placeholder name → selected value).
    fn resolve_template(
        &self,
        template: &TemplateEntry,
        tick: u64,
    ) -> (String, BTreeMap<String, String>) {
        let mut fields = BTreeMap::new();
        let mut message = template.message.clone();

        // Resolve each placeholder in the field_pools.
        for (field_name, pool) in &template.field_pools {
            let value = Self::select_from_pool(self.seed, tick, field_name, pool);
            fields.insert(field_name.clone(), value.to_string());
            let placeholder = format!("{{{field_name}}}");
            message = message.replace(&placeholder, value);
        }

        (message, fields)
    }
}

impl LogGenerator for LogTemplateGenerator {
    /// Generate a `LogEvent` for the given tick.
    ///
    /// Template selection is round-robin by tick index. Placeholder values and
    /// severity are selected via deterministic hash of `(seed, tick, name)`.
    fn generate(&self, tick: u64) -> LogEvent {
        if self.templates.is_empty() {
            return LogEvent::new(Severity::Info, String::new(), BTreeMap::new());
        }

        let template = &self.templates[(tick as usize) % self.templates.len()];
        let severity = self.select_severity(tick);
        let (message, fields) = self.resolve_template(template, tick);

        LogEvent::new(severity, message, fields)
    }
}
