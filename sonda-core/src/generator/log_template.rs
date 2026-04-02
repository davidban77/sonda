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
use crate::util::splitmix64;

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

    /// Produce a deterministic `u64` hash from the seed, tick, and a string discriminant.
    ///
    /// The discriminant is hashed character-by-character to avoid allocations.
    fn hash_for(seed: u64, tick: u64, discriminant: &str) -> u64 {
        // Fold discriminant bytes into the hash to make each field independent.
        let mut h = splitmix64(seed ^ tick);
        for b in discriminant.bytes() {
            h = splitmix64(h ^ (b as u64));
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
    /// Uses a single-pass scan: walks the template string once, copies literal
    /// segments directly into the output buffer, and when a `{name}` placeholder
    /// is encountered, looks up the pre-selected value from the fields map and
    /// writes it in place. This avoids the N successive `String::replace` calls
    /// (and their N intermediate `String` allocations) of the naive approach.
    ///
    /// Returns the resolved message string and the fields map (placeholder name
    /// to selected value).
    fn resolve_template(
        &self,
        template: &TemplateEntry,
        tick: u64,
    ) -> (String, BTreeMap<String, String>) {
        // Phase 1: select all field values up front. We need the fields map for
        // LogEvent::fields anyway, so this adds no extra work.
        let mut fields = BTreeMap::new();
        for (field_name, pool) in &template.field_pools {
            let value = Self::select_from_pool(self.seed, tick, field_name, pool);
            fields.insert(field_name.clone(), value.to_string());
        }

        // Phase 2: single-pass scan of the template string. Walk byte-by-byte,
        // copying literals and resolving `{name}` placeholders via lookup into
        // the fields map built above.
        let src = template.message.as_bytes();
        let len = src.len();
        let mut message = String::with_capacity(len);
        let mut i = 0;

        while i < len {
            if src[i] == b'{' {
                // Look for the matching closing brace.
                if let Some(close_offset) = src[i + 1..].iter().position(|&b| b == b'}') {
                    let name = &template.message[i + 1..i + 1 + close_offset];
                    if let Some(value) = fields.get(name) {
                        message.push_str(value);
                    } else {
                        // Not a known placeholder — copy the `{name}` literal.
                        message.push_str(&template.message[i..i + 1 + close_offset + 1]);
                    }
                    i += close_offset + 2; // skip past the closing '}'
                } else {
                    // No closing brace found — copy the '{' literally.
                    message.push('{');
                    i += 1;
                }
            } else {
                // Fast path: copy the longest literal run in one slice operation
                // instead of pushing one byte at a time.
                let start = i;
                while i < len && src[i] != b'{' {
                    i += 1;
                }
                message.push_str(&template.message[start..i]);
            }
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

        // Perform modulo in u64 space to avoid truncation on 32-bit platforms
        // where `usize` is 32 bits and ticks above u32::MAX would wrap silently.
        let template = &self.templates[(tick % self.templates.len() as u64) as usize];
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
    // Large tick values and 32-bit truncation safety
    // ---------------------------------------------------------------------------

    #[test]
    fn large_tick_does_not_panic() {
        let gen = make_simple_generator(1);
        let _ = gen.generate(u64::MAX);
        let _ = gen.generate(u64::MAX - 1);
    }

    #[test]
    fn tick_above_u32_max_selects_correct_template() {
        let entry_a = TemplateEntry {
            message: "template-A".into(),
            field_pools: HashMap::new(),
        };
        let entry_b = TemplateEntry {
            message: "template-B".into(),
            field_pools: HashMap::new(),
        };
        let entry_c = TemplateEntry {
            message: "template-C".into(),
            field_pools: HashMap::new(),
        };
        let gen = LogTemplateGenerator::new(vec![entry_a, entry_b, entry_c], vec![], 0);
        // tick = 4_294_967_296: u64 modulo 4_294_967_296 % 3 = 1
        let tick: u64 = u64::from(u32::MAX) + 1;
        assert_eq!(
            gen.generate(tick).message,
            "template-B",
            "tick {} mod 3 = 1, should select template-B",
            tick
        );
    }

    #[test]
    fn tick_at_u64_max_selects_correct_template() {
        let entry_a = TemplateEntry {
            message: "template-A".into(),
            field_pools: HashMap::new(),
        };
        let entry_b = TemplateEntry {
            message: "template-B".into(),
            field_pools: HashMap::new(),
        };
        let entry_c = TemplateEntry {
            message: "template-C".into(),
            field_pools: HashMap::new(),
        };
        let gen = LogTemplateGenerator::new(vec![entry_a, entry_b, entry_c], vec![], 0);
        // u64::MAX % 3 = 0
        assert_eq!(
            gen.generate(u64::MAX).message,
            "template-A",
            "u64::MAX % 3 = 0, should select template-A"
        );
    }

    // ---------------------------------------------------------------------------
    // Single-pass template resolution edge cases
    // ---------------------------------------------------------------------------

    #[test]
    fn template_with_no_placeholders_returns_literal() {
        let entry = TemplateEntry {
            message: "plain message with no placeholders".into(),
            field_pools: HashMap::new(),
        };
        let gen = LogTemplateGenerator::new(vec![entry], vec![], 0);
        let event = gen.generate(0);
        assert_eq!(event.message, "plain message with no placeholders");
        assert!(event.fields.is_empty());
    }

    #[test]
    fn template_with_unknown_placeholder_preserves_literal_braces() {
        // A {name} that is NOT in field_pools should be copied literally.
        let entry = TemplateEntry {
            message: "hello {unknown} world".into(),
            field_pools: HashMap::new(),
        };
        let gen = LogTemplateGenerator::new(vec![entry], vec![], 0);
        let event = gen.generate(0);
        assert_eq!(
            event.message, "hello {unknown} world",
            "unknown placeholders are preserved literally"
        );
    }

    #[test]
    fn template_with_unclosed_brace_copies_brace_literally() {
        let entry = TemplateEntry {
            message: "trailing open brace {".into(),
            field_pools: HashMap::new(),
        };
        let gen = LogTemplateGenerator::new(vec![entry], vec![], 0);
        let event = gen.generate(0);
        assert_eq!(
            event.message, "trailing open brace {",
            "unclosed brace at end is copied as-is"
        );
    }

    #[test]
    fn template_with_adjacent_placeholders_resolves_both() {
        let entry = TemplateEntry {
            message: "{a}{b}".into(),
            field_pools: {
                let mut m = HashMap::new();
                m.insert("a".into(), vec!["ALPHA".into()]);
                m.insert("b".into(), vec!["BETA".into()]);
                m
            },
        };
        let gen = LogTemplateGenerator::new(vec![entry], vec![], 0);
        let event = gen.generate(0);
        assert_eq!(event.message, "ALPHABETA");
    }

    #[test]
    fn template_with_repeated_placeholder_resolves_all_occurrences() {
        let entry = TemplateEntry {
            message: "{x} and {x} again".into(),
            field_pools: {
                let mut m = HashMap::new();
                m.insert("x".into(), vec!["VAL".into()]);
                m
            },
        };
        let gen = LogTemplateGenerator::new(vec![entry], vec![], 0);
        let event = gen.generate(0);
        assert_eq!(event.message, "VAL and VAL again");
    }

    #[test]
    fn template_with_placeholder_at_start_and_end() {
        let entry = TemplateEntry {
            message: "{start}middle{end}".into(),
            field_pools: {
                let mut m = HashMap::new();
                m.insert("start".into(), vec!["[BEGIN]".into()]);
                m.insert("end".into(), vec!["[END]".into()]);
                m
            },
        };
        let gen = LogTemplateGenerator::new(vec![entry], vec![], 0);
        let event = gen.generate(0);
        assert_eq!(event.message, "[BEGIN]middle[END]");
    }

    #[test]
    fn template_only_placeholder_no_literal_text() {
        let entry = TemplateEntry {
            message: "{sole}".into(),
            field_pools: {
                let mut m = HashMap::new();
                m.insert("sole".into(), vec!["ONLY".into()]);
                m
            },
        };
        let gen = LogTemplateGenerator::new(vec![entry], vec![], 0);
        let event = gen.generate(0);
        assert_eq!(event.message, "ONLY");
        assert_eq!(event.fields.get("sole").unwrap(), "ONLY");
    }

    #[test]
    fn template_empty_message_returns_empty_string() {
        let entry = TemplateEntry {
            message: String::new(),
            field_pools: HashMap::new(),
        };
        let gen = LogTemplateGenerator::new(vec![entry], vec![], 0);
        let event = gen.generate(0);
        assert_eq!(event.message, "");
    }

    #[test]
    fn template_mixed_known_and_unknown_placeholders() {
        let entry = TemplateEntry {
            message: "{known} then {mystery} then {known}".into(),
            field_pools: {
                let mut m = HashMap::new();
                m.insert("known".into(), vec!["K".into()]);
                m
            },
        };
        let gen = LogTemplateGenerator::new(vec![entry], vec![], 0);
        let event = gen.generate(0);
        assert_eq!(
            event.message, "K then {mystery} then K",
            "known placeholders resolve; unknown ones stay literal"
        );
    }

    #[test]
    fn template_with_empty_placeholder_name_preserved() {
        // `{}` is not a valid field name (no pool entry) so should be kept literally.
        let entry = TemplateEntry {
            message: "before {} after".into(),
            field_pools: HashMap::new(),
        };
        let gen = LogTemplateGenerator::new(vec![entry], vec![], 0);
        let event = gen.generate(0);
        assert_eq!(event.message, "before {} after");
    }

    #[test]
    fn fields_map_populated_even_if_placeholder_not_in_message() {
        // field_pools has "phantom" key but the message does not contain {phantom}.
        // The fields map should still contain it (pool selection happens unconditionally).
        let entry = TemplateEntry {
            message: "no placeholders here".into(),
            field_pools: {
                let mut m = HashMap::new();
                m.insert("phantom".into(), vec!["ghost".into()]);
                m
            },
        };
        let gen = LogTemplateGenerator::new(vec![entry], vec![], 0);
        let event = gen.generate(0);
        assert_eq!(event.message, "no placeholders here");
        assert_eq!(
            event.fields.get("phantom").unwrap(),
            "ghost",
            "fields map includes pool entries even when not referenced in template"
        );
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
