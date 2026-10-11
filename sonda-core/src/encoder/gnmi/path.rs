//! gNMI path templates and client path parsing.
//!
//! A template such as `/interfaces/interface[name={ifName}]/state/counters/{name}`
//! is parsed once into segments and rendered per series into a
//! [`proto::Path`].
//!
//! # Template grammar
//!
//! - Segments are separated by `/`; a leading `/` is optional.
//! - A template carries no `origin:` prefix; the origin comes from the
//!   encoder's `origin` field. A colon elsewhere — a module-prefixed element
//!   name or a key value — is literal text.
//! - A segment is `name` or `name[key=value]` with one or more `[key=value]`
//!   groups. Key names are literals, and a key appears at most once per
//!   segment.
//! - An element name or a key value is either a literal or exactly one
//!   placeholder `{x}`. A brace anywhere else is an error.
//! - `{name}` renders the metric name with `_` replaced by `-`. Any other
//!   `{label}` renders that label's value verbatim.

use std::collections::BTreeMap;

use super::proto;
use crate::model::metric::Labels;
use crate::{ConfigError, EncoderError, SondaError};

/// The placeholder that renders the metric name rather than a label.
pub const NAME_PLACEHOLDER: &str = "name";

/// A literal or a placeholder.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Part {
    Literal(String),
    Placeholder(String),
}

/// One `/`-separated element of a template.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Segment {
    name: Part,
    keys: Vec<(String, Part)>,
}

/// A parsed gNMI path template. See the module docs for the grammar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathTemplate {
    segments: Vec<Segment>,
}

impl PathTemplate {
    /// Parse a template string.
    ///
    /// Returns [`SondaError::Config`] naming the template and the problem for
    /// an empty template, an `origin:` prefix, an empty segment, a malformed
    /// `[key=value]` group, or a brace that is not a whole-part placeholder.
    pub fn parse(template: &str) -> Result<Self, SondaError> {
        let invalid = |reason: &str| {
            SondaError::Config(ConfigError::invalid(format!(
                "invalid gnmi path template {template:?}: {reason}"
            )))
        };

        if let (Some(origin), _) = split_origin(template) {
            return Err(invalid(&format!(
                "the template carries an origin prefix {origin:?}; set the encoder's \
                 `origin` field instead"
            )));
        }
        let body = template.strip_prefix('/').unwrap_or(template);
        if body.is_empty() {
            return Err(invalid("the path has no elements"));
        }

        let mut segments = Vec::new();
        for raw in split_outside_brackets(body) {
            segments.push(parse_segment(raw).map_err(|reason| invalid(&reason))?);
        }
        Ok(Self { segments })
    }

    /// Placeholders this template references, for validation against an entry's labels.
    ///
    /// Yields each placeholder name in template order, including
    /// [`NAME_PLACEHOLDER`] when present. Duplicates are yielded once per use.
    pub fn placeholders(&self) -> impl Iterator<Item = &str> {
        self.segments.iter().flat_map(|segment| {
            std::iter::once(&segment.name)
                .chain(segment.keys.iter().map(|(_, value)| value))
                .filter_map(|part| match part {
                    Part::Placeholder(p) => Some(p.as_str()),
                    Part::Literal(_) => None,
                })
        })
    }

    /// Placeholders in element-name position, in template order. Placeholders
    /// in key values are not included: those may render to any text.
    pub(crate) fn element_name_placeholders(&self) -> impl Iterator<Item = &str> {
        self.segments
            .iter()
            .filter_map(|segment| match &segment.name {
                Part::Placeholder(p) => Some(p.as_str()),
                Part::Literal(_) => None,
            })
    }

    /// Render into a `proto::Path` for this series. Allocates; called once per series.
    ///
    /// The returned path has empty `origin` and `target`. Returns
    /// [`EncoderError::EventRejected`] when a placeholder names a label that
    /// `labels` does not contain, or when a placeholder in element-name
    /// position resolves to an empty string or one containing `/`, `[` or
    /// `]`, which would not read back as a single element. Key values may
    /// contain any of these.
    pub fn render(&self, name: &str, labels: &Labels) -> Result<proto::Path, SondaError> {
        let mut elem = Vec::with_capacity(self.segments.len());
        for segment in &self.segments {
            let mut key = BTreeMap::new();
            for (k, v) in &segment.keys {
                key.insert(k.clone(), resolve(v, name, labels)?);
            }
            let element = resolve(&segment.name, name, labels)?;
            if !is_valid_element_name(&element) {
                return Err(SondaError::Encoder(EncoderError::EventRejected(format!(
                    "metric {name:?} renders gnmi path element {element:?}; an element name \
                     must be non-empty and contain no '/', '[' or ']'"
                ))));
            }
            elem.push(proto::PathElem { name: element, key });
        }
        Ok(proto::Path {
            origin: String::new(),
            elem,
            target: String::new(),
        })
    }
}

/// Whether `element` reads back as exactly one path element: non-empty, with
/// no `/`, `[` or `]`. [`PathTemplate::render`] rejects an element name that
/// fails it, and validation applies it to label values known in advance.
pub(crate) fn is_valid_element_name(element: &str) -> bool {
    !element.is_empty() && !element.contains(['/', '[', ']'])
}

/// Resolve one part of a segment for a series.
fn resolve(part: &Part, name: &str, labels: &Labels) -> Result<String, SondaError> {
    match part {
        Part::Literal(s) => Ok(s.clone()),
        Part::Placeholder(p) if p == NAME_PLACEHOLDER => Ok(name.replace('_', "-")),
        Part::Placeholder(p) => labels
            .iter()
            .find(|(k, _)| *k == p.as_str())
            .map(|(_, v)| v.to_string())
            .ok_or_else(|| {
                SondaError::Encoder(EncoderError::EventRejected(format!(
                    "gnmi path template placeholder {{{p}}} names a label that metric \
                     {name:?} does not carry"
                )))
            }),
    }
}

/// Split an `origin:` prefix off a path string.
///
/// The origin is the text before the first `:/`, and only when that text
/// contains no `/` or `[`, so a module-prefixed element name
/// (`/openconfig-interfaces:interfaces`) or a colon inside a key value is never
/// taken for one. Returns `(None, s)` when there is no prefix. Both
/// [`PathTemplate::parse`] and [`parse_client_path`] use this one rule.
fn split_origin(s: &str) -> (Option<&str>, &str) {
    match s.find(":/") {
        Some(i) if !s[..i].contains(['/', '[']) => (Some(&s[..i]), &s[i + 1..]),
        _ => (None, s),
    }
}

/// Split on `/` that are not inside `[...]`.
fn split_outside_brackets(s: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0usize;
    let mut start = 0;
    for (i, c) in s.char_indices() {
        match c {
            '[' => depth += 1,
            ']' => depth = depth.saturating_sub(1),
            '/' if depth == 0 => {
                parts.push(&s[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    parts.push(&s[start..]);
    parts
}

/// Parse one template segment: `name` followed by zero or more `[key=value]`.
fn parse_segment(raw: &str) -> Result<Segment, String> {
    if raw.is_empty() {
        return Err("empty path segment".to_string());
    }
    let (name, mut rest) = match raw.find('[') {
        Some(i) => (&raw[..i], &raw[i..]),
        None => (raw, ""),
    };
    if name.is_empty() {
        return Err(format!("segment {raw:?} has no element name"));
    }
    if name.contains(']') {
        return Err(format!("unmatched ']' in segment {raw:?}"));
    }
    let name = parse_part(name)?;

    let mut keys = Vec::new();
    while !rest.is_empty() {
        let inner = rest
            .strip_prefix('[')
            .ok_or_else(|| format!("expected '[' after ']' in segment {raw:?}"))?;
        let close = inner
            .find(']')
            .ok_or_else(|| format!("unclosed '[' in segment {raw:?}"))?;
        let group = &inner[..close];
        if group.contains('[') {
            return Err(format!("nested '[' in segment {raw:?}"));
        }
        let (key, value) = group
            .split_once('=')
            .ok_or_else(|| format!("key group [{group}] in segment {raw:?} has no '='"))?;
        if key.is_empty() {
            return Err(format!(
                "key group [{group}] in segment {raw:?} has an empty key"
            ));
        }
        if key.contains(['{', '}']) {
            return Err(format!(
                "key name {key:?} in segment {raw:?} must be a literal, not a placeholder"
            ));
        }
        if keys.iter().any(|(existing, _)| existing == key) {
            return Err(format!(
                "key {key:?} appears more than once in segment {raw:?}"
            ));
        }
        keys.push((key.to_string(), parse_part(value)?));
        rest = &inner[close + 1..];
    }
    Ok(Segment { name, keys })
}

/// Parse a literal or a whole-part `{placeholder}`.
fn parse_part(s: &str) -> Result<Part, String> {
    if let Some(inner) = s.strip_prefix('{').and_then(|r| r.strip_suffix('}')) {
        if inner.is_empty() || inner.contains(['{', '}']) {
            return Err(format!("malformed placeholder {s:?}"));
        }
        return Ok(Part::Placeholder(inner.to_string()));
    }
    if s.contains(['{', '}']) {
        return Err(format!(
            "{s:?} mixes text and braces; a placeholder must be the whole name or value"
        ));
    }
    Ok(Part::Literal(s.to_string()))
}

/// Parse a client-supplied path string ("/a/b[k=v]/c", "/a[k=*]") into `proto::Path`.
///
/// Accepts an optional `origin:` prefix (text before the first `:/` when it
/// contains no `/` or `[`), an optional leading `/`, and `[key=value]` groups
/// in which `\` escapes the next character. `*` as a key value or element
/// name is kept verbatim. `""` and `"/"` parse to a path with no elements.
/// Returns [`SondaError::Config`] for an empty element, an unclosed or
/// malformed key group, or an empty element name or key.
pub fn parse_client_path(s: &str) -> Result<proto::Path, SondaError> {
    let invalid = |reason: String| {
        SondaError::Config(ConfigError::invalid(format!(
            "invalid gnmi path {s:?}: {reason}"
        )))
    };

    let (origin, rest) = split_origin(s);
    let origin = origin.unwrap_or("");
    let body = rest.strip_prefix('/').unwrap_or(rest);

    let mut elem = Vec::new();
    if !body.is_empty() {
        let mut chars = body.chars().peekable();
        loop {
            let mut name = String::new();
            while let Some(&c) = chars.peek() {
                if c == '/' || c == '[' {
                    break;
                }
                if c == ']' {
                    return Err(invalid("unmatched ']'".to_string()));
                }
                name.push(c);
                chars.next();
            }
            if name.is_empty() {
                return Err(invalid("empty path element".to_string()));
            }

            let mut key = BTreeMap::new();
            while chars.peek() == Some(&'[') {
                chars.next();
                let mut k = String::new();
                loop {
                    match chars.next() {
                        Some('=') => break,
                        Some(']') | None => {
                            return Err(invalid(format!(
                                "key group in element {name:?} has no '='"
                            )))
                        }
                        Some(c) => k.push(c),
                    }
                }
                if k.is_empty() {
                    return Err(invalid(format!("empty key in element {name:?}")));
                }
                let mut v = String::new();
                loop {
                    match chars.next() {
                        Some(']') => break,
                        Some('\\') => match chars.next() {
                            Some(c) => v.push(c),
                            None => {
                                return Err(invalid(format!("unclosed '[' in element {name:?}")))
                            }
                        },
                        Some(c) => v.push(c),
                        None => return Err(invalid(format!("unclosed '[' in element {name:?}"))),
                    }
                }
                if key.contains_key(&k) {
                    return Err(invalid(format!(
                        "key {k:?} appears more than once in element {name:?}"
                    )));
                }
                key.insert(k, v);
            }

            elem.push(proto::PathElem { name, key });
            match chars.next() {
                None => break,
                Some('/') => continue,
                Some(c) => {
                    return Err(invalid(format!(
                        "unexpected {c:?} after a key group in element {:?}",
                        elem.last().map(|e| e.name.as_str()).unwrap_or_default()
                    )))
                }
            }
        }
    }

    Ok(proto::Path {
        origin: origin.to_string(),
        elem,
        target: String::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    fn elem(name: &str, keys: &[(&str, &str)]) -> proto::PathElem {
        proto::PathElem {
            name: name.to_string(),
            key: keys
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        }
    }

    fn labels() -> Labels {
        Labels::from_pairs(&[
            ("ifName", "GigabitEthernet0/0/0"),
            ("device", "rtr-1"),
            ("subif", "0"),
            ("odd", "a]b\\c"),
            ("leaf", "in-octets"),
        ])
        .unwrap()
    }

    /// Render `template` for metric `metric` against [`labels`], then check
    /// the element list, and for every successful render that
    /// `parse_client_path(render.to_string())` gives the same path back.
    #[rustfmt::skip]
    #[rstest]
    #[case::plain(               "/system/state/hostname",                                    "m",            Ok(vec![elem("system", &[]), elem("state", &[]), elem("hostname", &[])]))]
    #[case::no_leading_slash(    "system/state",                                              "m",            Ok(vec![elem("system", &[]), elem("state", &[])]))]
    #[case::one_key(             "/interfaces/interface[name={ifName}]/state",                "m",            Ok(vec![elem("interfaces", &[]), elem("interface", &[("name", "GigabitEthernet0/0/0")]), elem("state", &[])]))]
    #[case::two_keys(            "/interfaces/interface[name={ifName}][index={subif}]/state", "m",            Ok(vec![elem("interfaces", &[]), elem("interface", &[("index", "0"), ("name", "GigabitEthernet0/0/0")]), elem("state", &[])]))]
    #[case::literal_key_value(   "/network-instances/network-instance[name=default]",         "m",            Ok(vec![elem("network-instances", &[]), elem("network-instance", &[("name", "default")])]))]
    #[case::placeholder_element( "/interfaces/interface[name={ifName}]/state/counters/{leaf}", "m",           Ok(vec![elem("interfaces", &[]), elem("interface", &[("name", "GigabitEthernet0/0/0")]), elem("state", &[]), elem("counters", &[]), elem("in-octets", &[])]))]
    #[case::name_underscore(     "/state/counters/{name}",                                    "in_octets_x",  Ok(vec![elem("state", &[]), elem("counters", &[]), elem("in-octets-x", &[])]))]
    #[case::escaped_value(       "/a[k={odd}]",                                               "m",            Ok(vec![elem("a", &[("k", "a]b\\c")])]))]
    #[case::missing_label(       "/interfaces/interface[name={nope}]",                        "m",            Err("{nope}"))]
    #[case::malformed_bracket(   "/interfaces/interface[name={ifName}/state",                 "m",            Err("unclosed '['"))]
    #[case::missing_equals(      "/interfaces/interface[name]",                               "m",            Err("has no '='"))]
    #[case::empty_segment(       "/interfaces//state",                                        "m",            Err("empty path segment"))]
    #[case::trailing_slash(      "/interfaces/state/",                                        "m",            Err("empty path segment"))]
    #[case::empty_template(      "/",                                                         "m",            Err("no elements"))]
    #[case::mixed_braces(        "/a/x{ifName}",                                              "m",            Err("mixes text and braces"))]
    #[case::empty_placeholder(   "/a/{}",                                                     "m",            Err("malformed placeholder"))]
    #[case::repeated_key(        "/a[k={ifName}][k={device}]",                                "m",            Err("appears more than once"))]
    #[case::slash_in_element(    "/interfaces/{ifName}/state",                                "m",            Err("element name"))]
    #[case::bracket_in_element(  "/a/{odd}",                                                  "m",            Err("element name"))]
    #[case::origin_prefix(       "openconfig:/interfaces/state",                              "m",            Err("origin prefix \"openconfig\""))]
    #[case::module_prefixed_elem("/openconfig-interfaces:interfaces/state",                   "m",            Ok(vec![elem("openconfig-interfaces:interfaces", &[]), elem("state", &[])]))]
    #[case::colon_in_key_value(  "/a[k=Ethernet1:1]",                                         "m",            Ok(vec![elem("a", &[("k", "Ethernet1:1")])]))]
    #[case::colon_slash_in_key(  "/a[k=x:/y]",                                                "m",            Ok(vec![elem("a", &[("k", "x:/y")])]))]
    fn template_renders(
        #[case] template: &str,
        #[case] metric: &str,
        #[case] expected: Result<Vec<proto::PathElem>, &str>,
    ) {
        let rendered = PathTemplate::parse(template).and_then(|t| t.render(metric, &labels()));
        match (rendered, expected) {
            (Ok(path), Ok(elems)) => {
                assert_eq!(path.elem, elems);
                assert!(path.origin.is_empty() && path.target.is_empty());
                let text = path.to_string();
                let parsed = parse_client_path(&text)
                    .unwrap_or_else(|e| panic!("{text:?} did not parse back: {e}"));
                assert_eq!(parsed, path, "round trip through {text:?}");
            }
            (Err(e), Err(needle)) => {
                let msg = e.to_string();
                assert!(msg.contains(needle), "error {msg:?} should contain {needle:?}");
            }
            (got, want) => panic!("template {template:?}: got {got:?}, want {want:?}"),
        }
    }

    /// An empty label value in element position would render `/a//b`, which
    /// `parse_client_path` itself rejects. In key position it is fine.
    #[test]
    fn empty_label_value_is_rejected_only_as_an_element_name() {
        let labels = Labels::from_pairs(&[("e", "")]).unwrap();
        let as_key = PathTemplate::parse("/a[k={e}]/b").unwrap();
        as_key
            .render("m", &labels)
            .expect("an empty key value renders");
        let as_element = PathTemplate::parse("/a/{e}/b").unwrap();
        let err = as_element.render("m", &labels).unwrap_err();
        assert!(
            matches!(err, SondaError::Encoder(EncoderError::EventRejected(_))),
            "{err:?}"
        );
    }

    #[test]
    fn missing_label_rejects_the_event_and_bad_grammar_is_a_config_error() {
        let template = PathTemplate::parse("/a[k={nope}]").unwrap();
        assert!(matches!(
            template.render("m", &labels()),
            Err(SondaError::Encoder(EncoderError::EventRejected(_)))
        ));
        assert!(matches!(
            PathTemplate::parse("/a[k"),
            Err(SondaError::Config(_))
        ));
    }

    #[test]
    fn placeholders_lists_names_and_key_values_in_order() {
        let template =
            PathTemplate::parse("/{root}/interface[name={ifName}][kind=eth]/state/{name}").unwrap();
        let got: Vec<&str> = template.placeholders().collect();
        assert_eq!(got, vec!["root", "ifName", "name"]);
    }

    #[test]
    fn element_name_placeholders_skip_key_values() {
        let t = PathTemplate::parse("/a[k={key}]/{elem}/{name}/c[x={other}]").unwrap();
        let names: Vec<&str> = t.element_name_placeholders().collect();
        assert_eq!(names, ["elem", "name"]);
    }

    #[rstest]
    #[case::plain("Gi0", true)]
    #[case::empty("", false)]
    #[case::slash("Gi0/0/0", false)]
    #[case::open_bracket("a[b", false)]
    #[case::close_bracket("a]b", false)]
    fn element_name_rule(#[case] element: &str, #[case] valid: bool) {
        assert_eq!(is_valid_element_name(element), valid);
    }

    #[test]
    fn placeholders_is_empty_for_a_literal_template() {
        let template = PathTemplate::parse("/a/b[k=v]").unwrap();
        assert_eq!(template.placeholders().count(), 0);
    }

    #[test]
    fn template_key_names_must_be_literals() {
        let err = PathTemplate::parse("/a[{k}=v]").unwrap_err().to_string();
        assert!(err.contains("must be a literal"), "{err}");
    }

    #[test]
    fn client_path_with_origin_and_wildcard() {
        let path = parse_client_path("openconfig:/interfaces/interface[name=*]/state").unwrap();
        assert_eq!(path.origin, "openconfig");
        assert_eq!(
            path.elem,
            vec![
                elem("interfaces", &[]),
                elem("interface", &[("name", "*")]),
                elem("state", &[]),
            ]
        );
        assert_eq!(
            path.to_string(),
            "openconfig:/interfaces/interface[name=*]/state"
        );
    }

    #[test]
    fn client_path_key_value_may_contain_slashes_and_escapes() {
        let path = parse_client_path(r"/a[k=Gi0/0/0][j=x\]y]/b").unwrap();
        assert_eq!(
            path.elem,
            vec![elem("a", &[("j", "x]y"), ("k", "Gi0/0/0")]), elem("b", &[])]
        );
    }

    #[test]
    fn client_path_module_prefixed_element_is_not_an_origin() {
        let path = parse_client_path("/openconfig-interfaces:interfaces/interface").unwrap();
        assert_eq!(path.origin, "");
        assert_eq!(path.elem[0].name, "openconfig-interfaces:interfaces");
    }

    #[rstest]
    #[case::empty("")]
    #[case::root("/")]
    #[case::origin_only("openconfig:/")]
    fn client_root_paths_have_no_elements(#[case] input: &str) {
        let path = parse_client_path(input).unwrap();
        assert!(path.elem.is_empty(), "{input:?} -> {path:?}");
    }

    #[rustfmt::skip]
    #[rstest]
    #[case::empty_element(   "/a//b",      "empty path element")]
    #[case::trailing_slash(  "/a/",        "empty path element")]
    #[case::unclosed(        "/a[k=v",     "unclosed '['")]
    #[case::no_equals(       "/a[k]",      "has no '='")]
    #[case::empty_key(       "/a[=v]",     "empty key")]
    #[case::stray_close(     "/a]",        "unmatched ']'")]
    #[case::junk_after_key(  "/a[k=v]x",   "unexpected 'x'")]
    #[case::bare_key_group(  "[k=*]",      "empty path element")]
    #[case::repeated_key(    "/a[k=1][k=2]", "appears more than once")]
    fn client_path_errors(#[case] input: &str, #[case] needle: &str) {
        let err = parse_client_path(input).unwrap_err();
        assert!(matches!(err, SondaError::Config(_)));
        let msg = err.to_string();
        assert!(msg.contains(needle), "{input:?}: {msg:?} should contain {needle:?}");
    }
}
