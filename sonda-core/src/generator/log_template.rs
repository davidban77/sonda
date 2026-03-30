//! Template-based log generator — produces structured log events from message
//! templates with field pools.
//!
//! All selection (template, placeholder values, severity) is deterministic:
//! given the same `seed` and `tick`, `generate()` always produces an identical
//! `LogEvent`. No mutable state is required, satisfying the `&self` contract of
//! [`LogGenerator`].

use std::collections::{BTreeMap, HashMap};

use crate::model::log::{LogEvent, Severity};
use crate::model::metric::Labels;

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
            return LogEvent::new(
                Severity::Info,
                String::new(),
                Labels::default(),
                BTreeMap::new(),
            );
        }

        let template = &self.templates[(tick as usize) % self.templates.len()];
        let severity = self.select_severity(tick);
        let (message, fields) = self.resolve_template(template, tick);

        LogEvent::new(severity, message, Labels::default(), fields)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Build a simple single-template generator for reuse across tests.
    fn make_simple_generator(seed: u64) -> LogTemplateGenerator {
        let entry = TemplateEntry {
            message: "Request from {ip} to {endpoint}".into(),
            field_pools: {
                let mut m = HashMap::new();
                m.insert(
                    "ip".into(),
                    vec!["10.0.0.1".into(), "10.0.0.2".into(), "10.0.0.3".into()],
                );
                m.insert("endpoint".into(), vec!["/api".into(), "/health".into()]);
                m
            },
        };
        LogTemplateGenerator::new(vec![entry], vec![], seed)
    }

    // Build a generator with explicit severity weights.
    fn make_weighted_generator(seed: u64) -> LogTemplateGenerator {
        let entry = TemplateEntry {
            message: "msg".into(),
            field_pools: HashMap::new(),
        };
        let weights = vec![
            (Severity::Info, 0.7),
            (Severity::Warn, 0.2),
            (Severity::Error, 0.1),
        ];
        LogTemplateGenerator::new(vec![entry], weights, seed)
    }

    // ---------------------------------------------------------------------------
    // Determinism tests
    // ---------------------------------------------------------------------------

    #[test]
    fn same_seed_and_tick_produce_identical_message() {
        let gen = make_simple_generator(42);
        let event_a = gen.generate(0);
        let event_b = gen.generate(0);
        assert_eq!(
            event_a.message, event_b.message,
            "same tick must yield identical message"
        );
    }

    #[test]
    fn same_seed_and_tick_produce_identical_severity() {
        let gen = make_simple_generator(42);
        let event_a = gen.generate(5);
        let event_b = gen.generate(5);
        assert_eq!(
            event_a.severity, event_b.severity,
            "same tick must yield identical severity"
        );
    }

    #[test]
    fn same_seed_and_tick_produce_identical_fields() {
        let gen = make_simple_generator(99);
        let event_a = gen.generate(17);
        let event_b = gen.generate(17);
        assert_eq!(
            event_a.fields, event_b.fields,
            "same tick must yield identical fields map"
        );
    }

    #[test]
    fn different_seeds_produce_different_output_for_same_tick() {
        let gen_a = make_simple_generator(1);
        let gen_b = make_simple_generator(2);
        // It is astronomically unlikely that two different seeds produce identical
        // resolved messages across all of these ticks.
        let mut all_same = true;
        for tick in 0..20 {
            if gen_a.generate(tick).message != gen_b.generate(tick).message {
                all_same = false;
                break;
            }
        }
        assert!(
            !all_same,
            "different seeds should produce at least one differing message"
        );
    }

    // ---------------------------------------------------------------------------
    // Placeholder resolution tests
    // ---------------------------------------------------------------------------

    #[test]
    fn resolved_ip_value_comes_from_pool() {
        let gen = make_simple_generator(42);
        let pool: Vec<&str> = vec!["10.0.0.1", "10.0.0.2", "10.0.0.3"];
        for tick in 0..50 {
            let event = gen.generate(tick);
            let ip = event
                .fields
                .get("ip")
                .expect("fields must contain 'ip' key");
            assert!(
                pool.contains(&ip.as_str()),
                "ip value {:?} at tick {} not in pool {:?}",
                ip,
                tick,
                pool
            );
        }
    }

    #[test]
    fn resolved_endpoint_value_comes_from_pool() {
        let gen = make_simple_generator(42);
        let pool: Vec<&str> = vec!["/api", "/health"];
        for tick in 0..50 {
            let event = gen.generate(tick);
            let ep = event
                .fields
                .get("endpoint")
                .expect("fields must contain 'endpoint' key");
            assert!(
                pool.contains(&ep.as_str()),
                "endpoint value {:?} at tick {} not in pool {:?}",
                ep,
                tick,
                pool
            );
        }
    }

    #[test]
    fn resolved_message_contains_no_unresolved_placeholders() {
        let gen = make_simple_generator(7);
        for tick in 0..50 {
            let event = gen.generate(tick);
            assert!(
                !event.message.contains('{'),
                "message {:?} at tick {} still has unresolved placeholder",
                event.message,
                tick
            );
        }
    }

    #[test]
    fn resolved_message_contains_selected_field_value() {
        let gen = make_simple_generator(42);
        for tick in 0..20 {
            let event = gen.generate(tick);
            let ip = event.fields.get("ip").expect("ip must be present");
            let ep = event
                .fields
                .get("endpoint")
                .expect("endpoint must be present");
            assert!(
                event.message.contains(ip.as_str()),
                "message {:?} must contain ip {:?}",
                event.message,
                ip
            );
            assert!(
                event.message.contains(ep.as_str()),
                "message {:?} must contain endpoint {:?}",
                event.message,
                ep
            );
        }
    }

    // ---------------------------------------------------------------------------
    // Round-robin template selection
    // ---------------------------------------------------------------------------

    #[test]
    fn two_templates_selected_round_robin() {
        let entry_a = TemplateEntry {
            message: "template-A".into(),
            field_pools: HashMap::new(),
        };
        let entry_b = TemplateEntry {
            message: "template-B".into(),
            field_pools: HashMap::new(),
        };
        let gen = LogTemplateGenerator::new(vec![entry_a, entry_b], vec![], 0);

        assert_eq!(
            gen.generate(0).message,
            "template-A",
            "tick 0 should select template 0"
        );
        assert_eq!(
            gen.generate(1).message,
            "template-B",
            "tick 1 should select template 1"
        );
        assert_eq!(
            gen.generate(2).message,
            "template-A",
            "tick 2 should wrap to template 0"
        );
        assert_eq!(
            gen.generate(3).message,
            "template-B",
            "tick 3 should select template 1"
        );
    }

    // ---------------------------------------------------------------------------
    // Severity default behavior
    // ---------------------------------------------------------------------------

    #[test]
    fn empty_severity_weights_defaults_to_info() {
        let gen = make_simple_generator(0);
        // With no explicit weights the factory sets vec![(Info, 1.0)].
        for tick in 0..20 {
            let event = gen.generate(tick);
            assert_eq!(
                event.severity,
                Severity::Info,
                "default weights should always yield Info at tick {tick}"
            );
        }
    }

    // ---------------------------------------------------------------------------
    // Severity weight distribution
    // ---------------------------------------------------------------------------

    #[test]
    fn severity_distribution_matches_weights_within_five_percent() {
        // Spec: info=0.7 / warn=0.2 / error=0.1 over 10,000 ticks within 5%.
        let gen = make_weighted_generator(0);
        let n = 10_000u64;
        let mut info_count = 0u64;
        let mut warn_count = 0u64;
        let mut error_count = 0u64;

        for tick in 0..n {
            match gen.generate(tick).severity {
                Severity::Info => info_count += 1,
                Severity::Warn => warn_count += 1,
                Severity::Error => error_count += 1,
                other => panic!("unexpected severity {:?} at tick {tick}", other),
            }
        }

        let info_ratio = info_count as f64 / n as f64;
        let warn_ratio = warn_count as f64 / n as f64;
        let error_ratio = error_count as f64 / n as f64;

        assert!(
            (info_ratio - 0.7).abs() < 0.05,
            "info ratio {info_ratio:.3} not within 5% of 0.7"
        );
        assert!(
            (warn_ratio - 0.2).abs() < 0.05,
            "warn ratio {warn_ratio:.3} not within 5% of 0.2"
        );
        assert!(
            (error_ratio - 0.1).abs() < 0.05,
            "error ratio {error_ratio:.3} not within 5% of 0.1"
        );
    }

    // ---------------------------------------------------------------------------
    // Empty templates edge case
    // ---------------------------------------------------------------------------

    #[test]
    fn empty_templates_returns_empty_info_event() {
        let gen = LogTemplateGenerator::new(vec![], vec![], 0);
        let event = gen.generate(0);
        assert_eq!(event.severity, Severity::Info);
        assert_eq!(event.message, "");
        assert!(event.fields.is_empty());
    }

    // ---------------------------------------------------------------------------
    // Large tick values
    // ---------------------------------------------------------------------------

    #[test]
    fn large_tick_does_not_panic() {
        let gen = make_simple_generator(1);
        let _ = gen.generate(u64::MAX);
        let _ = gen.generate(u64::MAX - 1);
    }

    // ---------------------------------------------------------------------------
    // Send + Sync contract
    // ---------------------------------------------------------------------------

    fn assert_send_sync<T: Send + Sync>() {}

    #[test]
    fn log_template_generator_is_send_and_sync() {
        assert_send_sync::<LogTemplateGenerator>();
    }
}
