use sonda_parsers::rawlog::{run, RawlogArgs};
use std::path::PathBuf;
use tempfile::TempDir;

const NGINX_FIXTURE: &str = r#"192.168.1.10 - - [10/Oct/2024:13:55:36 +0000] "GET /api/v1/users HTTP/1.1" 200 1234 "-" "Mozilla/5.0"
192.168.1.11 - - [10/Oct/2024:13:55:38 +0000] "POST /api/v1/login HTTP/1.1" 401 12 "-" "curl/7.79"
192.168.1.12 - - [10/Oct/2024:13:55:40 +0000] "GET /api/v1/missing HTTP/1.1" 404 0 "-" "Mozilla/5.0"
192.168.1.13 - - [10/Oct/2024:13:55:42 +0000] "GET /api/v1/health HTTP/1.1" 200 5 "-" "checker/1.0"
192.168.1.14 - - [10/Oct/2024:13:55:44 +0000] "POST /api/v1/orders HTTP/1.1" 500 2048 "-" "kit/2.0"
"#;

fn write_input(dir: &TempDir, name: &str, content: &str) -> PathBuf {
    let p = dir.path().join(name);
    std::fs::write(&p, content).unwrap();
    p
}

#[test]
fn end_to_end_nginx_replay_produces_loadable_yaml_and_correct_csv() {
    let dir = TempDir::new().unwrap();
    let input = write_input(&dir, "sample.log", NGINX_FIXTURE);
    let yaml_out = dir.path().join("scenario.yaml");

    let result = run(RawlogArgs {
        input,
        format: "nginx".to_string(),
        output: yaml_out.clone(),
        delta_seconds: None,
        scenario_name: Some("test_nginx".to_string()),
    })
    .expect("nginx parse must succeed");

    assert_eq!(result.row_count, 5);
    assert_eq!(result.format, "nginx");

    let csv = std::fs::read_to_string(&result.csv_path).unwrap();
    let lines: Vec<&str> = csv.lines().collect();
    assert_eq!(lines.len(), 6, "expected header + 5 data rows");
    assert_eq!(
        lines[0],
        "timestamp,severity,message,method,path,remote_addr,status,user_agent"
    );

    assert!(lines[1].starts_with("1728568536,info,GET /api/v1/users HTTP/1.1 200"));
    assert!(lines[2].starts_with("1728568538,warn,POST /api/v1/login HTTP/1.1 401"));
    assert!(lines[3].starts_with("1728568540,warn,GET /api/v1/missing HTTP/1.1 404"));
    assert!(lines[5].starts_with("1728568544,error,POST /api/v1/orders HTTP/1.1 500"));

    let yaml = std::fs::read_to_string(&result.yaml_path).unwrap();
    use sonda_core::compile_scenario_file;
    use sonda_core::compiler::expand::InMemoryPackResolver;
    let resolver = InMemoryPackResolver::new();
    compile_scenario_file(&yaml, &resolver).expect("emitted scenario YAML must compile");
}

#[test]
fn end_to_end_plain_replay_synthesizes_timestamps_at_delta() {
    let dir = TempDir::new().unwrap();
    let input = write_input(
        &dir,
        "plain.log",
        "first line\nsecond line\nthird line\nfourth line\nfifth line\n",
    );
    let yaml_out = dir.path().join("plain.yaml");

    let result = run(RawlogArgs {
        input,
        format: "plain".to_string(),
        output: yaml_out.clone(),
        delta_seconds: Some(2.0),
        scenario_name: None,
    })
    .expect("plain parse must succeed");

    assert_eq!(result.row_count, 5);

    let csv = std::fs::read_to_string(&result.csv_path).unwrap();
    let lines: Vec<&str> = csv.lines().collect();
    assert_eq!(lines[0], "timestamp,severity,message");
    assert_eq!(lines[1], "1700000000,,first line");
    assert_eq!(lines[2], "1700000002,,second line");
    assert_eq!(lines[3], "1700000004,,third line");
    assert_eq!(lines[4], "1700000006,,fourth line");
    assert_eq!(lines[5], "1700000008,,fifth line");

    let yaml = std::fs::read_to_string(&result.yaml_path).unwrap();
    assert!(yaml.contains("name: plain_replay"));

    use sonda_core::compile_scenario_file;
    use sonda_core::compiler::expand::InMemoryPackResolver;
    let resolver = InMemoryPackResolver::new();
    compile_scenario_file(&yaml, &resolver).expect("emitted scenario YAML must compile");
}

#[test]
fn nginx_unparseable_lines_are_silently_skipped() {
    let dir = TempDir::new().unwrap();
    let mixed = format!(
        "{}garbage line\n{}",
        NGINX_FIXTURE,
        r#"192.168.1.15 - - [10/Oct/2024:13:55:46 +0000] "PUT /api/v1/x HTTP/1.1" 204 0 "-" "x""#
    );
    let input = write_input(&dir, "mixed.log", &mixed);
    let yaml_out = dir.path().join("mixed.yaml");
    let result = run(RawlogArgs {
        input,
        format: "nginx".to_string(),
        output: yaml_out,
        delta_seconds: None,
        scenario_name: None,
    })
    .unwrap();
    assert_eq!(result.row_count, 6, "garbage line must be skipped");
}

#[test]
fn emitted_csv_path_lives_next_to_yaml() {
    let dir = TempDir::new().unwrap();
    let input = write_input(&dir, "p.log", "a\nb\n");
    let yaml_out = dir.path().join("subdir/out.yaml");
    std::fs::create_dir_all(yaml_out.parent().unwrap()).unwrap();
    let result = run(RawlogArgs {
        input,
        format: "plain".to_string(),
        output: yaml_out.clone(),
        delta_seconds: None,
        scenario_name: None,
    })
    .unwrap();
    assert_eq!(result.csv_path.parent(), yaml_out.parent());

    let yaml_text = std::fs::read_to_string(&result.yaml_path).unwrap();
    assert!(
        yaml_text.contains("file: p.csv"),
        "expected relative csv path 'p.csv' (input stem), got: {yaml_text}"
    );
}
