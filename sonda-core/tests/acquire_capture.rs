//! Drive the capture path end to end against a real socket.
//!
//! Everything else in `acquire` is unit-tested without a network, which leaves
//! one seam untested: the HTTP client itself, and what it does with a
//! credential. These tests stand a throwaway TCP server in front of it so the
//! request is inspectable and the whole chain — fetch, normalize, CSV, scenario,
//! compile — runs on data that arrived over the wire.

#![cfg(all(feature = "http", feature = "config"))]

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc;
use std::time::Duration;

use sonda_core::acquire::normalize::{normalize, Grid};
use sonda_core::acquire::tsdb::{Auth, TsdbClient};
use sonda_core::acquire::{csv_out, yaml_out, FetchedSeries};
use sonda_core::compiler::expand::InMemoryPackResolver;

/// Serve `body` to exactly one request and hand back what that request was.
///
/// The captured text is what makes the leak tests non-vacuous: asserting a token
/// is absent from an artifact proves nothing unless the token was actually sent.
fn mock_tsdb(status: &str, body: &str) -> (String, mpsc::Receiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind an ephemeral port");
    let base = format!("http://{}", listener.local_addr().expect("addr"));
    let (tx, rx) = mpsc::channel();
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );

    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        // Read until the header terminator; the client sends no body.
        let mut seen = Vec::new();
        let mut byte = [0u8; 1];
        while stream.read(&mut byte).unwrap_or(0) == 1 {
            seen.push(byte[0]);
            if seen.ends_with(b"\r\n\r\n") {
                break;
            }
        }
        let _ = stream.write_all(response.as_bytes());
        let _ = stream.flush();
        let _ = tx.send(String::from_utf8_lossy(&seen).into_owned());
    });

    (base, rx)
}

fn matrix(label_block: &str, samples: &str) -> String {
    format!(
        r#"{{"status":"success","data":{{"resultType":"matrix","result":[
             {{"metric":{label_block},"values":[{samples}]}}]}}}}"#
    )
}

fn fetch(base: &str, auth: Auth) -> Result<Vec<FetchedSeries>, sonda_core::SondaError> {
    TsdbClient::new(base, auth, Duration::from_secs(5)).fetch_range(
        "up",
        0.0,
        30.0,
        Duration::from_secs(10),
    )
}

/// Fetch, normalize, write both halves, and load them with the real compiler.
#[test]
fn a_capture_taken_over_http_replays_through_the_real_compiler() {
    let body = matrix(
        r#"{"__name__":"up","job":"api"}"#,
        // 10s grid over 0..=30 with the sample at 10 missing.
        r#"[0,"1"],[20,"3"],[30,"4"]"#,
    );
    let (base, _rx) = mock_tsdb("200 OK", &body);

    let series = fetch(&base, Auth::None).expect("fetch must succeed");
    assert_eq!(series.len(), 1, "one series came back");

    let grid = Grid::new(0.0, 30.0, 10.0).expect("grid");
    let normalized: Vec<_> = series.iter().map(|s| normalize(s, grid)).collect();
    assert_eq!(normalized[0].gap_count(), 1, "the missing sample is a gap");

    let dir = tempfile::tempdir().expect("tempdir");
    let csv_path = dir.path().join("capture.csv");
    std::fs::write(
        &csv_path,
        csv_out::write_csv(grid, &normalized).expect("csv"),
    )
    .expect("write");

    let file = yaml_out::scenario_for(csv_path.to_str().expect("utf8"), grid, &normalized)
        .expect("scenario");
    let yaml = yaml_out::to_yaml(&file).expect("yaml");

    let entries = sonda_core::compile_scenario_file(&yaml, &InMemoryPackResolver::default())
        .unwrap_or_else(|e| panic!("the compiler rejected the capture: {e}\n{yaml}"));
    for e in entries {
        sonda_core::config::expand_entry(e).expect("the emitted columns must expand");
    }
}

/// The credential reaches the server and appears in nothing this path writes.
#[test]
fn a_bearer_token_is_sent_and_never_reaches_an_emitted_artifact() {
    const TOKEN: &str = "s3cr3t-bearer-value-do-not-emit";

    let body = matrix(r#"{"__name__":"up","job":"api"}"#, r#"[0,"1"],[10,"2"]"#);
    let (base, rx) = mock_tsdb("200 OK", &body);

    let series = fetch(&base, Auth::Bearer(TOKEN.to_string())).expect("fetch");

    // Vacuity guard. Without this the assertions below pass just as well when
    // the client forgets to send the credential at all.
    let request = rx.recv_timeout(Duration::from_secs(5)).expect("request");
    assert!(
        request.contains(TOKEN),
        "the token must actually have been sent, or the leak checks prove nothing:\n{request}"
    );
    assert!(
        request.contains(&format!("Bearer {TOKEN}")),
        "and sent as a Bearer header:\n{request}"
    );

    let grid = Grid::new(0.0, 10.0, 10.0).expect("grid");
    let normalized: Vec<_> = series.iter().map(|s| normalize(s, grid)).collect();

    let csv = csv_out::write_csv(grid, &normalized).expect("csv");
    let file = yaml_out::scenario_for("capture.csv", grid, &normalized).expect("scenario");
    let yaml = yaml_out::to_yaml(&file).expect("yaml");

    for (what, text) in [("csv", &csv), ("yaml", &yaml)] {
        assert!(
            !text.contains(TOKEN),
            "the {what} artifact carries the credential:\n{text}"
        );
    }

    // The client's own Debug is the other place a credential escapes to a log.
    let client = TsdbClient::new(
        &base,
        Auth::Bearer(TOKEN.to_string()),
        Duration::from_secs(1),
    );
    assert!(!format!("{client:?}").contains(TOKEN));
}

/// A failing fetch reports the URL, not the credential.
#[test]
fn an_error_response_does_not_quote_the_credential_back() {
    const TOKEN: &str = "s3cr3t-in-an-error-path";
    let (base, rx) = mock_tsdb(
        "500 Internal Server Error",
        r#"{"status":"error","errorType":"internal","error":"boom"}"#,
    );

    let err = fetch(&base, Auth::Bearer(TOKEN.to_string())).expect_err("a 500 must be an error");

    let request = rx.recv_timeout(Duration::from_secs(5)).expect("request");
    assert!(request.contains(TOKEN), "the token was sent");

    let text = err.to_string();
    assert!(
        !text.contains(TOKEN),
        "the error quotes the credential: {text}"
    );
    assert!(
        text.contains("/api/v1/query_range"),
        "and it should still name the endpoint: {text}"
    );
}

/// Basic auth is base64-encoded, so the raw password must be checked for too —
/// and so must the encoding, which is what would actually appear in a header.
#[test]
fn basic_auth_credentials_do_not_reach_an_artifact_in_either_form() {
    const PASSWORD: &str = "hunter2-plaintext";

    let body = matrix(r#"{"__name__":"up"}"#, r#"[0,"1"],[10,"2"]"#);
    let (base, rx) = mock_tsdb("200 OK", &body);

    let series = fetch(
        &base,
        Auth::Basic {
            user: "admin".to_string(),
            password: PASSWORD.to_string(),
        },
    )
    .expect("fetch");

    let request = rx.recv_timeout(Duration::from_secs(5)).expect("request");
    let encoded = request
        .lines()
        .find_map(|l| l.strip_prefix("Authorization: Basic "))
        .expect("a Basic header was sent")
        .trim()
        .to_string();
    // Vacuity guard, and it has to be this exact value: a header that is merely
    // present and non-empty is satisfied by base64("admin:"), so the plaintext
    // check below would pass with the password never leaving the process.
    assert_eq!(
        encoded, "YWRtaW46aHVudGVyMi1wbGFpbnRleHQ=",
        "the password must actually have been sent, or the plaintext leak check proves nothing"
    );

    let grid = Grid::new(0.0, 10.0, 10.0).expect("grid");
    let normalized: Vec<_> = series.iter().map(|s| normalize(s, grid)).collect();
    let csv = csv_out::write_csv(grid, &normalized).expect("csv");
    let yaml = yaml_out::to_yaml(
        &yaml_out::scenario_for("capture.csv", grid, &normalized).expect("scenario"),
    )
    .expect("yaml");

    for (what, text) in [("csv", &csv), ("yaml", &yaml)] {
        assert!(!text.contains(PASSWORD), "{what} carries the password");
        assert!(
            !text.contains(&encoded),
            "{what} carries the encoded header"
        );
    }
}

/// `http://user:pass@host` is the other way a credential reaches this client,
/// and it is the one that is stored rather than applied and dropped.
#[test]
fn a_credential_in_the_base_url_reaches_neither_the_error_nor_the_debug() {
    const PASSWORD: &str = "urlsecret9999";

    let (base, rx) = mock_tsdb(
        "500 Internal Server Error",
        r#"{"status":"error","error":"boom"}"#,
    );
    let with_credential = base.replace("http://", &format!("http://admin:{PASSWORD}@"));

    let err = fetch(&with_credential, Auth::None).expect_err("a 500 must be an error");

    // The credential still has to be on the wire, or nothing below is a test.
    let request = rx.recv_timeout(Duration::from_secs(5)).expect("request");
    assert!(
        request.starts_with("GET "),
        "the request was actually made:\n{request}"
    );

    let text = err.to_string();
    assert!(!text.contains(PASSWORD), "the error quotes it: {text}");
    // Same trap as the basic-auth guard: absence proves nothing if the URL
    // never carried a credential. The marker says one was there and was cut.
    assert!(
        text.contains("<redacted>@"),
        "a credential was redacted, rather than there being none: {text}"
    );
    assert!(
        text.contains("/api/v1/query_range"),
        "and it still names the endpoint: {text}"
    );

    let client = TsdbClient::new(&with_credential, Auth::None, Duration::from_secs(1));
    let shown = format!("{client:?}");
    assert!(!shown.contains(PASSWORD), "Debug carries it: {shown}");
    assert!(
        shown.contains("<redacted>@"),
        "Debug shows the cut: {shown}"
    );
}

/// Labels chosen to break the header grammar, arriving over the wire.
#[test]
fn hostile_labels_from_the_server_survive_into_a_loadable_capture() {
    // Quote, backslash, brace, comma and a unicode value — each one is a
    // character the label block or the CSV field has to escape.
    let body = matrix(
        r#"{"__name__":"up","quote":"a\"b","backslash":"a\\b","brace":"{v}","comma":"a,b","unicode":"héllo → ✓"}"#,
        r#"[0,"1"],[10,"2"],[20,"3"]"#,
    );
    let (base, _rx) = mock_tsdb("200 OK", &body);

    let series = fetch(&base, Auth::None).expect("fetch");
    let grid = Grid::new(0.0, 20.0, 10.0).expect("grid");
    let normalized: Vec<_> = series.iter().map(|s| normalize(s, grid)).collect();

    let dir = tempfile::tempdir().expect("tempdir");
    let csv_path = dir.path().join("hostile.csv");
    std::fs::write(
        &csv_path,
        csv_out::write_csv(grid, &normalized).expect("csv"),
    )
    .expect("write");

    let yaml = yaml_out::to_yaml(
        &yaml_out::scenario_for(csv_path.to_str().expect("utf8"), grid, &normalized)
            .expect("scenario"),
    )
    .expect("yaml");

    let entries = sonda_core::compile_scenario_file(&yaml, &InMemoryPackResolver::default())
        .unwrap_or_else(|e| panic!("hostile labels broke the capture: {e}\n{yaml}"));
    let runnables: usize = entries
        .into_iter()
        .map(|e| sonda_core::config::expand_entry(e).expect("expand").len())
        .sum();
    assert_eq!(runnables, 1, "one series, one runnable");

    // And the values survived the two escaping layers intact.
    for (k, v) in [
        ("quote", "a\"b"),
        ("backslash", "a\\b"),
        ("brace", "{v}"),
        ("comma", "a,b"),
        ("unicode", "héllo → ✓"),
    ] {
        assert_eq!(
            normalized[0].labels.get(k).map(String::as_str),
            Some(v),
            "label {k} arrived intact"
        );
    }
}
