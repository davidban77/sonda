//! `GET /snmp/agents`: the SNMP agents this process serves, with their
//! request counters.

use std::fmt::Write as _;
use std::time::SystemTime;

use axum::Json;
use chrono::{DateTime, SecondsFormat, Utc};
use serde::Serialize;
use sonda_core::sink::snmp::{registry, LastRequest, RequestCounters, SnmpAgent};

/// Body of `GET /snmp/agents`.
#[derive(Debug, Serialize)]
pub struct AgentsResponse {
    /// Every live agent, sorted by listen address.
    pub agents: Vec<AgentBody>,
}

/// One agent.
#[derive(Debug, Serialize)]
pub struct AgentBody {
    /// The address the agent listens on, with an ephemeral port resolved.
    pub listen: String,
    /// The SNMP versions the agent answers, ascending: `v1`, `v2c`, `v3`.
    pub versions: Vec<&'static str>,
    /// Whether the agent answers SNMPv1 or SNMPv2c, whose requests must
    /// carry its community.
    pub community_required: bool,
    /// The agent's SNMPv3 engine ID, lower-case hex.
    pub engine_id: String,
    /// The agent's SNMPv3 user names, sorted.
    pub users: Vec<String>,
    /// Instances sinks have published, the system group not included.
    pub instances: usize,
    /// Whole seconds since the agent bound its socket.
    pub uptime_seconds: u64,
    /// The requests the agent has received, by kind.
    pub requests: RequestsBody,
    /// The last datagram the agent received; `null` before the first.
    pub last_request: Option<LastRequestBody>,
}

/// Declares [`RequestsBody`] with one `u64` per named [`RequestCounters`]
/// field, and its `From<RequestCounters>`, which copies each field from the
/// counter of the same name.
macro_rules! requests_body {
    ($($field:ident),+ $(,)?) => {
        /// The requests an agent has received, by kind: the
        /// [`RequestCounters`] fields, by their names.
        #[derive(Debug, Serialize)]
        pub struct RequestsBody {
            $(
                #[doc = concat!("[`RequestCounters::", stringify!($field), "`].")]
                pub $field: u64,
            )+
        }

        impl From<RequestCounters> for RequestsBody {
            fn from(counters: RequestCounters) -> Self {
                Self {
                    $($field: counters.$field,)+
                }
            }
        }
    };
}

requests_body!(
    get,
    get_next,
    get_bulk,
    set_refused,
    malformed,
    wrong_community,
    too_big,
    unsupported_version,
    send_errors,
    v1,
    v3,
    usm_unknown_engine_ids,
    usm_unknown_user_names,
    usm_unsupported_sec_levels,
    usm_wrong_digests,
    usm_not_in_time_windows,
    usm_decryption_errors,
);

/// The last datagram an agent received.
#[derive(Debug, Serialize)]
pub struct LastRequestBody {
    /// When it arrived, RFC 3339 UTC to the second.
    pub at: String,
    /// Where it came from.
    pub peer: String,
    /// The SNMP version its message names, `v1`, `v2c` or `v3`; `null`
    /// when it is not an SNMP message.
    pub version: Option<&'static str>,
}

/// `GET /snmp/agents` → 200 with every live agent in the process.
pub async fn list_agents() -> Json<AgentsResponse> {
    Json(AgentsResponse {
        agents: registry()
            .targets()
            .iter()
            .map(|agent| agent_body(agent))
            .collect(),
    })
}

fn agent_body(agent: &SnmpAgent) -> AgentBody {
    AgentBody {
        listen: agent.local_addr().to_string(),
        versions: agent.versions().iter().map(|v| v.name()).collect(),
        community_required: agent.community_required(),
        engine_id: hex(agent.engine_id()),
        users: agent.user_names(),
        instances: agent.instance_count(),
        uptime_seconds: agent.uptime().as_secs(),
        requests: agent.requests().into(),
        last_request: agent.last_request().as_ref().map(last_request_body),
    }
}

fn last_request_body(last: &LastRequest) -> LastRequestBody {
    LastRequestBody {
        at: rfc3339(last.at),
        peer: last.peer.to_string(),
        version: last.version.map(|v| v.name()),
    }
}

/// `bytes` as lower-case hex, without a prefix.
fn hex(bytes: &[u8]) -> String {
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(text, "{byte:02x}");
    }
    text
}

/// `time` as RFC 3339 UTC to the second, e.g. `2026-10-22T10:00:00Z`.
fn rfc3339(time: SystemTime) -> String {
    DateTime::<Utc>::from(time).to_rfc3339_opts(SecondsFormat::Secs, true)
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;
    use std::time::{Duration, UNIX_EPOCH};

    use http_body_util::BodyExt;
    use hyper::{Request, StatusCode};
    use serde_json::{json, Value};
    use sonda_core::ScenarioHandle;
    use tokio::net::UdpSocket;
    use tower::ServiceExt;

    use super::*;
    use crate::routes::router;
    use crate::state::AppState;

    const PATIENCE: Duration = Duration::from_secs(5);

    /// The SNMPv3 user's passwords, which no body may carry.
    const AUTH_PASSWORD: &str = "route-auth-secret";
    const PRIV_PASSWORD: &str = "route-priv-secret";

    /// `snmpget -v2c -c public 127.0.0.1:1161 1.3.6.1.2.1.1.3.0`, net-snmp 5.9.4.
    const NET_SNMP_GET: &str =
        "302902010104067075626c6963a01c02043ac25c77020100020100300e300c06082b060102010103\
        000500";

    /// The page documenting the route, from the crate root.
    const PAGE: &str = "../docs/site/docs/deploy/http-api.md";

    /// The route's section heading on [`PAGE`].
    const SECTION: &str = "### `GET /snmp/agents`";

    fn unhex(text: &str) -> Vec<u8> {
        let digits: Vec<u8> = text.bytes().filter(u8::is_ascii_hexdigit).collect();
        digits
            .chunks(2)
            .map(|pair| {
                let pair = std::str::from_utf8(pair).expect("hex digits are ASCII");
                u8::from_str_radix(pair, 16).expect("hex digits")
            })
            .collect()
    }

    /// `GET /snmp/agents` through the router: the body as text and as JSON.
    async fn get_agents(state: &AppState) -> (String, Value) {
        let request = Request::builder()
            .uri("/snmp/agents")
            .body(axum::body::Body::empty())
            .expect("request");
        let response = router(state.clone())
            .oneshot(request)
            .await
            .expect("infallible");
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("body")
            .to_bytes();
        let text = String::from_utf8(bytes.to_vec()).expect("a UTF-8 body");
        let json = serde_json::from_str(&text).expect("a JSON body");
        (text, json)
    }

    /// Poll `GET /snmp/agents` until `done` holds of its body; the last body
    /// read.
    async fn agents_until(
        state: &AppState,
        mut done: impl FnMut(&Value) -> bool,
    ) -> (String, Value) {
        let deadline = tokio::time::Instant::now() + PATIENCE;
        loop {
            let (text, body) = get_agents(state).await;
            if done(&body) || tokio::time::Instant::now() >= deadline {
                return (text, body);
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// The agent listening on `listen` in a body, or `Null`.
    fn agent_on(body: &Value, listen: SocketAddr) -> &Value {
        body["agents"]
            .as_array()
            .and_then(|agents| {
                agents
                    .iter()
                    .find(|agent| agent["listen"] == listen.to_string())
            })
            .unwrap_or(&Value::Null)
    }

    /// Whether every counter of an agent's `requests` is 0; `false` for no
    /// counters.
    fn all_zero(requests: &Value) -> bool {
        requests
            .as_object()
            .is_some_and(|counters| !counters.is_empty() && counters.values().all(|n| n == 0))
    }

    /// Stop and join every scenario in `state`.
    async fn stop_all(state: &AppState) {
        let handles: Vec<ScenarioHandle> =
            state.scenarios.write().drain().map(|(_, h)| h).collect();
        for mut handle in handles {
            handle.stop();
            let _ = handle.join_async(Some(PATIENCE)).await;
        }
    }

    /// Two scenarios started through `POST /scenarios`, each with an
    /// `snmp_agent` on `127.0.0.1:0`, the second answering SNMPv3 for one
    /// user. The route lists both in address order with their versions, user
    /// names (never a password), engine IDs and instances. A GET from a test
    /// socket shows on the first agent's counters and as its last request,
    /// with the socket's address and the version; a datagram that is not
    /// SNMP shows as malformed, with no version. Both agents are gone once
    /// the scenarios stop.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_agents_route_lists_each_agent_and_its_requests() {
        let _registry = crate::routes::SNMP_REGISTRY.lock().await;
        assert!(
            registry().targets().is_empty(),
            "the premise: no agent is running"
        );

        let yaml = format!(
            "\
version: 2
kind: runnable
defaults:
  rate: 20
  encoder:
    type: snmp
    oids: {{ in_octets: \"1.3.6.1.2.1.31.1.1.1.6.{{ifIndex}}\" }}
    types: {{ in_octets: counter64 }}
scenarios:
  - signal_type: metrics
    name: in_octets
    generator: {{ type: constant, value: 1.0 }}
    labels: {{ device: plain, ifIndex: 1 }}
    sink:
      type: snmp_agent
      listen: \"127.0.0.1:0\"
  - signal_type: metrics
    name: in_octets
    generator: {{ type: constant, value: 1.0 }}
    labels: {{ device: secured, ifIndex: 1 }}
    sink:
      type: snmp_agent
      listen: \"127.0.0.1:0\"
      versions: [v2c, v3]
      users:
        - name: poller
          auth: {{ protocol: sha256, password: {AUTH_PASSWORD} }}
          priv: {{ protocol: aes128, password: {PRIV_PASSWORD} }}
"
        );
        let state = AppState::new();
        let request = Request::builder()
            .method("POST")
            .uri("/scenarios")
            .header("content-type", "application/x-yaml")
            .body(axum::body::Body::from(yaml))
            .expect("request");
        let response = router(state.clone())
            .oneshot(request)
            .await
            .expect("infallible");
        assert_eq!(response.status(), StatusCode::CREATED);

        let (text, body) = agents_until(&state, |body| {
            body["agents"].as_array().is_some_and(|agents| {
                agents.len() == 2 && agents.iter().all(|a| a["instances"] == 1)
            })
        })
        .await;
        let agents = body["agents"].as_array().expect("an agents array");
        assert_eq!(agents.len(), 2, "both agents: {body}");
        let listens: Vec<SocketAddr> = agents
            .iter()
            .map(|a| {
                a["listen"]
                    .as_str()
                    .expect("listen")
                    .parse()
                    .expect("an address")
            })
            .collect();
        assert!(listens[0] < listens[1], "address order: {listens:?}");
        let (plain, secured) = if agents[0]["versions"] == json!(["v2c"]) {
            (&agents[0], &agents[1])
        } else {
            (&agents[1], &agents[0])
        };
        assert_eq!(plain["versions"], json!(["v2c"]));
        assert_eq!(secured["versions"], json!(["v2c", "v3"]));
        assert_eq!(plain["users"], json!([]));
        assert_eq!(secured["users"], json!(["poller"]));
        assert!(text.contains("\"poller\""), "the user is listed: {text}");
        for password in [AUTH_PASSWORD, PRIV_PASSWORD] {
            assert!(!text.contains(password), "a password in the body: {text}");
        }
        for agent in agents {
            assert_eq!(agent["community_required"], true, "{agent}");
            assert_eq!(agent["instances"], 1, "{agent}");
            let engine_id = agent["engine_id"].as_str().expect("an engine ID");
            assert_eq!(engine_id.len(), 42, "21 octets: {engine_id}");
            assert!(
                engine_id
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
                "lower-case hex: {engine_id}"
            );
            assert!(agent["uptime_seconds"].is_u64(), "{agent}");
            assert!(all_zero(&agent["requests"]), "no request yet: {agent}");
            assert_eq!(agent["last_request"], Value::Null, "{agent}");
        }
        assert_ne!(plain["engine_id"], secured["engine_id"]);

        let plain_listen: SocketAddr = plain["listen"]
            .as_str()
            .expect("listen")
            .parse()
            .expect("an address");
        let secured_listen: SocketAddr = secured["listen"]
            .as_str()
            .expect("listen")
            .parse()
            .expect("an address");
        let get = unhex(NET_SNMP_GET);
        assert_eq!(get.first(), Some(&0x30), "fixture present");
        let poller = UdpSocket::bind("127.0.0.1:0").await.expect("bind a poller");
        let before = SystemTime::now();
        poller.send_to(&get, plain_listen).await.expect("sent");
        let mut reply = [0; 1500];
        let (_, from) = tokio::time::timeout(PATIENCE, poller.recv_from(&mut reply))
            .await
            .expect("the GET is answered in time")
            .expect("received");
        let after = SystemTime::now();
        assert_eq!(from, plain_listen);

        let (_, body) = get_agents(&state).await;
        let plain = agent_on(&body, plain_listen);
        assert_eq!(plain["requests"]["get"], 1, "{plain}");
        let counted: u64 = plain["requests"]
            .as_object()
            .expect("counters")
            .values()
            .filter_map(Value::as_u64)
            .sum();
        assert_eq!(counted, 1, "the GET alone: {plain}");
        let last = &plain["last_request"];
        assert_eq!(
            last["peer"],
            poller.local_addr().expect("an address").to_string()
        );
        assert_eq!(last["version"], "v2c");
        let at = last["at"].as_str().expect("a time");
        assert!(at.ends_with('Z'), "UTC: {at}");
        let at = DateTime::parse_from_rfc3339(at)
            .unwrap_or_else(|e| panic!("{at}: {e}"))
            .timestamp();
        let seconds = |time: SystemTime| {
            time.duration_since(UNIX_EPOCH)
                .expect("after the epoch")
                .as_secs() as i64
        };
        assert!(
            seconds(before) <= at && at <= seconds(after),
            "read between the send and the answer: {last}"
        );
        let secured = agent_on(&body, secured_listen);
        assert!(all_zero(&secured["requests"]), "{secured}");
        assert_eq!(secured["last_request"], Value::Null, "{secured}");

        let stranger = UdpSocket::bind("127.0.0.1:0").await.expect("bind a socket");
        stranger
            .send_to(b"not an snmp message", plain_listen)
            .await
            .expect("sent");
        let (_, body) = agents_until(&state, |body| {
            agent_on(body, plain_listen)["requests"]["malformed"] == 1
        })
        .await;
        let plain = agent_on(&body, plain_listen);
        assert_eq!(plain["requests"]["malformed"], 1, "{plain}");
        assert_eq!(plain["requests"]["get"], 1, "{plain}");
        let last = &plain["last_request"];
        assert_eq!(
            last["peer"],
            stranger.local_addr().expect("an address").to_string()
        );
        assert_eq!(last["version"], Value::Null, "not SNMP: {last}");

        stop_all(&state).await;
        let (_, body) = agents_until(&state, |body| body["agents"] == json!([])).await;
        assert_eq!(body["agents"], json!([]), "stopped agents are gone");
    }

    /// The [`RequestCounters`] field names, in declaration order, read from
    /// its derived `Debug`, which the compiler writes from the struct.
    fn counter_names() -> Vec<String> {
        let debug = format!("{:?}", RequestCounters::default());
        let fields = debug
            .strip_prefix("RequestCounters { ")
            .and_then(|fields| fields.strip_suffix(" }"))
            .unwrap_or_else(|| panic!("not a derived Debug: {debug}"));
        fields
            .split(", ")
            .map(|field| {
                let (name, _) = field
                    .split_once(": ")
                    .unwrap_or_else(|| panic!("not `name: value`: {field}"));
                name.to_string()
            })
            .collect()
    }

    /// The `requests` object's keys are the [`RequestCounters`] fields, in
    /// the struct's order, so a counter added to it cannot be left out.
    #[test]
    fn the_requests_body_names_every_counter() {
        let names = counter_names();
        assert!(names.len() > 1, "the fields were read: {names:?}");
        let body = serde_json::to_string(&RequestsBody::from(RequestCounters::default()))
            .expect("serializes");
        let keys: Vec<String> = body
            .strip_prefix('{')
            .and_then(|pairs| pairs.strip_suffix('}'))
            .unwrap_or_else(|| panic!("not an object: {body}"))
            .split(',')
            .map(|pair| {
                let (key, _) = pair
                    .split_once(':')
                    .unwrap_or_else(|| panic!("not `key:value`: {pair}"));
                key.trim_matches('"').to_string()
            })
            .collect();
        assert_eq!(keys, names);
    }

    /// An agent body with every field present, `last_request` included.
    fn full_body() -> Value {
        serde_json::to_value(AgentBody {
            listen: "127.0.0.1:1161".to_string(),
            versions: vec!["v2c"],
            community_required: true,
            engine_id: String::new(),
            users: Vec::new(),
            instances: 0,
            uptime_seconds: 0,
            requests: RequestCounters::default().into(),
            last_request: Some(LastRequestBody {
                at: String::new(),
                peer: String::new(),
                version: None,
            }),
        })
        .expect("serializes")
    }

    /// An agent's key paths, sorted: each key, and `key.child` for each key
    /// of an object it holds.
    fn key_paths(agent: &Value) -> Vec<String> {
        let mut paths = Vec::new();
        for (key, value) in agent.as_object().expect("an object") {
            paths.push(key.clone());
            if let Some(children) = value.as_object() {
                paths.extend(children.keys().map(|child| format!("{key}.{child}")));
            }
        }
        paths.sort();
        paths
    }

    /// The route's section of [`PAGE`]: from its heading to the next
    /// heading of its level or above.
    fn documented_section() -> String {
        let page = std::fs::read_to_string(PAGE)
            .unwrap_or_else(|e| panic!("cannot read {PAGE}: {e} — has the page moved?"));
        let start = page
            .find(SECTION)
            .unwrap_or_else(|| panic!("{PAGE} has no {SECTION} section"));
        let section = &page[start + SECTION.len()..];
        let end = ["\n## ", "\n### "]
            .iter()
            .filter_map(|heading| section.find(heading))
            .min()
            .unwrap_or(section.len());
        section[..end].to_string()
    }

    /// The section's response example documents exactly the body's keys,
    /// and its field table has one row per key, the `requests` counters in
    /// the struct's order.
    #[test]
    fn the_documented_response_has_the_body_s_keys() {
        let section = documented_section();
        let fence = "```json title=\"Response\"";
        let start = section
            .find(fence)
            .unwrap_or_else(|| panic!("the {SECTION} section has no {fence} fence"));
        let example = &section[start + fence.len()..];
        let end = example
            .find("\n```")
            .unwrap_or_else(|| panic!("the {SECTION} response fence is unterminated"));
        let example: Value = serde_json::from_str(&example[..end])
            .unwrap_or_else(|e| panic!("the {SECTION} response is not JSON: {e}"));
        let expected = key_paths(&full_body());

        let keys: Vec<&String> = example.as_object().expect("an object").keys().collect();
        assert_eq!(keys, ["agents"]);
        let agents = example["agents"].as_array().expect("an agents array");
        assert!(
            !agents.is_empty(),
            "the example lists no agent: this check would pass on nothing"
        );
        for agent in agents {
            assert_eq!(key_paths(agent), expected, "{agent}");
        }

        let rows: Vec<&str> = section
            .lines()
            .filter_map(|line| line.strip_prefix("| `"))
            .filter_map(|row| row.split_once('`'))
            .map(|(field, _)| field)
            .collect();
        let mut sorted = rows.clone();
        sorted.sort_unstable();
        assert_eq!(sorted, expected, "one row per field");
        let counters: Vec<&str> = rows
            .iter()
            .filter_map(|row| row.strip_prefix("requests."))
            .collect();
        assert_eq!(counters, counter_names(), "in the struct's order");
    }
}
