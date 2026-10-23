//! Throwaway SNMPv2c responder over the BER codec in `src/encoder/snmp/`.
//!
//! Serves the system group, 40 ifXTable instances (columns 6 and 10 as
//! Counter64, 16 and 17 as INTEGER, indices 1-10) whose values advance each
//! second, and a playpen subtree under 1.3.6.1.4.1.8072.9999.1 holding BER
//! edge values. Answers GET, GETNEXT and GETBULK into a size budget; SET is
//! refused with notWritable; anything else is dropped.
//!
//! ```text
//! cargo run -p sonda-core --example snmp_spike -- [--listen 127.0.0.1:1161]
//!     [--budget 1472] [--community public] [--hex]
//! ```

use std::collections::BTreeMap;
use std::net::UdpSocket;
use std::ops::Bound;
use std::time::Instant;

#[allow(dead_code)]
#[path = "../src/encoder/snmp/ber.rs"]
mod ber;
#[allow(dead_code)]
#[path = "../src/encoder/snmp/oid.rs"]
mod oid;

use ber::{Message, Tag};
use oid::Oid;

const SYS_UPTIME: &str = "1.3.6.1.2.1.1.3.0";
const IFX_ENTRY: &str = "1.3.6.1.2.1.31.1.1.1";
const PLAYPEN: &str = "1.3.6.1.4.1.8072.9999.1";
const NOT_WRITABLE: i64 = 17;
const TOO_BIG: i64 = 1;

struct Args {
    listen: String,
    budget: usize,
    community: String,
    hex: bool,
}

fn parse_args(mut args: impl Iterator<Item = String>) -> Result<Args, String> {
    let mut parsed = Args {
        listen: "127.0.0.1:1161".to_owned(),
        budget: 1472,
        community: "public".to_owned(),
        hex: false,
    };
    while let Some(flag) = args.next() {
        let mut value = || args.next().ok_or(format!("{flag} needs a value"));
        match flag.as_str() {
            "--listen" => parsed.listen = value()?,
            "--budget" => parsed.budget = value()?.parse().map_err(|e| format!("--budget: {e}"))?,
            "--community" => parsed.community = value()?,
            "--hex" => parsed.hex = true,
            other => return Err(format!("unknown argument {other}")),
        }
    }
    Ok(parsed)
}

fn oid(text: &str) -> Oid {
    Oid::parse(text).unwrap_or_else(|e| panic!("spike OID {text}: {e}"))
}

fn signed(v: i64) -> Vec<u8> {
    let mut out = Vec::new();
    ber::write_i64(&mut out, Tag::Integer, v);
    out
}

fn unsigned(tag: Tag, v: u64) -> Vec<u8> {
    let mut out = Vec::new();
    ber::write_u64(&mut out, tag, v);
    out
}

fn octets(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    ber::write_octets(&mut out, Tag::OctetString, bytes);
    out
}

fn oid_value(value: &Oid) -> Vec<u8> {
    let mut out = Vec::new();
    ber::write_oid(&mut out, value);
    out
}

/// The table, keyed by OID in walk order, values as encoded TLVs.
struct Agent {
    start: Instant,
    second: Option<u64>,
    table: BTreeMap<Oid, Vec<u8>>,
    /// Object types (columns and scalars): a name under one that is not in
    /// the table is noSuchInstance, any other noSuchObject.
    objects: Vec<Oid>,
}

impl Agent {
    fn new() -> Self {
        let mut agent = Agent {
            start: Instant::now(),
            second: None,
            table: BTreeMap::new(),
            objects: Vec::new(),
        };
        let system: [(&str, Vec<u8>); 7] = [
            ("1.3.6.1.2.1.1.1", octets(b"Sonda SNMP spike responder")),
            (
                "1.3.6.1.2.1.1.2",
                oid_value(&oid("1.3.6.1.4.1.8072.3.2.10")),
            ),
            ("1.3.6.1.2.1.1.3", unsigned(Tag::TimeTicks, 0)),
            ("1.3.6.1.2.1.1.4", octets(b"noc@example.com")),
            ("1.3.6.1.2.1.1.5", octets(b"rtr-spike-01")),
            ("1.3.6.1.2.1.1.6", octets(b"lab-1")),
            ("1.3.6.1.2.1.1.7", signed(72)),
        ];
        for (object, value) in system {
            agent.objects.push(oid(object));
            agent.table.insert(oid(&format!("{object}.0")), value);
        }
        for column in [6, 10, 16, 17] {
            agent.objects.push(oid(&format!("{IFX_ENTRY}.{column}")));
        }
        let playpen: [(&str, Vec<u8>); 18] = [
            ("1.0", signed(-1)),
            ("2.0", signed(-128)),
            ("3.0", signed(-129)),
            ("4.0", signed(i64::from(i32::MIN))),
            ("5.0", signed(i64::from(i32::MAX))),
            ("6.0", unsigned(Tag::Counter64, 1 << 63)),
            ("7.0", unsigned(Tag::Counter64, u64::MAX)),
            ("8.0", unsigned(Tag::Counter32, u64::from(u32::MAX))),
            ("9.0", unsigned(Tag::Gauge32, 0)),
            ("10.0", unsigned(Tag::TimeTicks, u64::from(u32::MAX))),
            ("11.0", octets(&[b'a'; 127])),
            ("12.0", octets(&[b'b'; 128])),
            ("13.0", octets(&[b'c'; 300])),
            ("14.0", oid_value(&oid("1.3.6.1.4.1.4294967295.16384"))),
            ("15.0", signed(1 << 40)),
            ("16.0", unsigned(Tag::Counter32, 1 << 40)),
            ("17.16384", signed(1)),
            ("17.4294967295", signed(2)),
        ];
        for (suffix, value) in playpen {
            let instance = oid(&format!("{PLAYPEN}.{suffix}"));
            let object = Oid::from_arcs(instance.arcs()[..instance.arcs().len() - 1].to_vec())
                .expect("playpen object encodable");
            if !agent.objects.contains(&object) {
                agent.objects.push(object);
            }
            agent.table.insert(instance, value);
        }
        agent.refresh();
        agent
    }

    /// Re-encode sysUpTime on every request and the ifXTable values when the
    /// elapsed second changes.
    fn refresh(&mut self) {
        let elapsed = self.start.elapsed();
        let ticks = (elapsed.as_millis() / 10) as u64 & u64::from(u32::MAX);
        self.table
            .insert(oid(SYS_UPTIME), unsigned(Tag::TimeTicks, ticks));
        let t = elapsed.as_secs();
        if self.second == Some(t) {
            return;
        }
        self.second = Some(t);
        for index in 1..=10u64 {
            // Index 10 crosses 2^63 at t = 4; index 9 of column 10 nears u64::MAX.
            let in_octets = if index == 10 {
                ((1u64 << 63) - 500_000).wrapping_add(125_000 * t)
            } else {
                index * 1_000 + index * 125_000 * t
            };
            let out_octets = if index == 9 {
                (u64::MAX - 10_000_000).wrapping_add(250_000 * t)
            } else {
                index * 62_500 * t
            };
            // ifPromiscuousMode: index 1 flaps true(1)/false(2) each second.
            let promiscuous = if index == 1 && t % 2 == 1 { 1 } else { 2 };
            let rows: [(u32, Vec<u8>); 4] = [
                (6, unsigned(Tag::Counter64, in_octets)),
                (10, unsigned(Tag::Counter64, out_octets)),
                (16, signed(promiscuous)),
                (17, signed(1)),
            ];
            for (column, value) in rows {
                self.table
                    .insert(oid(&format!("{IFX_ENTRY}.{column}.{index}")), value);
            }
        }
    }

    /// The value of `name`, or the exception GET answers with.
    fn get(&self, name: &Oid) -> Vec<u8> {
        if let Some(value) = self.table.get(name) {
            return value.clone();
        }
        let tag = if self.objects.iter().any(|object| object.is_prefix_of(name)) {
            Tag::NoSuchInstance
        } else {
            Tag::NoSuchObject
        };
        vec![tag.byte(), 0]
    }

    /// The smallest instance strictly after `name`.
    fn next_after(&self, name: &Oid) -> Option<(&Oid, &Vec<u8>)> {
        self.table
            .range::<Oid, _>((Bound::Excluded(name), Bound::Unbounded))
            .next()
    }
}

/// What happened to a datagram, for the log line.
enum Outcome {
    Sent(String),
    Dropped(String),
}

/// Append `name = value` to `varbinds` if the whole response still fits
/// `budget`; false when it would not.
fn push_varbind(
    varbinds: &mut Vec<u8>,
    request: &Message<'_>,
    budget: usize,
    name: &Oid,
    value: &[u8],
) -> bool {
    let grown = varbinds.len() + ber::varbind_len(name, value.len());
    if ber::message_len(
        request.version,
        request.community,
        request.request_id,
        0,
        0,
        grown,
    ) > budget
    {
        return false;
    }
    ber::write_varbind(varbinds, name, value);
    true
}

fn answer(
    agent: &mut Agent,
    datagram: &[u8],
    community: &[u8],
    budget: usize,
    out: &mut Vec<u8>,
) -> Outcome {
    out.clear();
    let request = match Message::decode(datagram) {
        Ok(request) => request,
        Err(e) => return Outcome::Dropped(format!("malformed: {e}")),
    };
    if request.version != 1 {
        return Outcome::Dropped(format!("version {} (only v2c)", request.version));
    }
    if request.community != community {
        return Outcome::Dropped(format!(
            "wrong community ({} bytes)",
            request.community.len()
        ));
    }
    let names: Vec<Oid> = match request
        .varbinds()
        .map(|vb| vb.and_then(|vb| vb.name()))
        .collect()
    {
        Ok(names) => names,
        Err(e) => return Outcome::Dropped(format!("malformed varbind: {e}")),
    };
    agent.refresh();
    let mut varbinds = Vec::new();
    let mut error = (0, 0);
    let summary;
    match request.pdu {
        Tag::GetRequest | Tag::GetNextRequest => {
            for name in &names {
                let (name, value) = if request.pdu == Tag::GetRequest {
                    (name.clone(), agent.get(name))
                } else {
                    match agent.next_after(name) {
                        Some((next, value)) => (next.clone(), value.clone()),
                        None => (name.clone(), vec![Tag::EndOfMibView.byte(), 0]),
                    }
                };
                if !push_varbind(&mut varbinds, &request, budget, &name, &value) {
                    varbinds.clear();
                    error = (TOO_BIG, 0);
                    break;
                }
            }
            summary = format!(
                "{} {} varbind(s){}",
                if request.pdu == Tag::GetRequest {
                    "GET"
                } else {
                    "GETNEXT"
                },
                names.len(),
                if error.0 == TOO_BIG { " -> tooBig" } else { "" }
            );
        }
        Tag::GetBulkRequest => {
            let non_repeaters = request.error_status.clamp(0, names.len() as i64) as usize;
            let max_repetitions = request.error_index.max(0) as usize;
            let mut sent = 0;
            let mut trimmed = false;
            'fill: {
                for name in &names[..non_repeaters] {
                    let (next, value) = match agent.next_after(name) {
                        Some((next, value)) => (next.clone(), value.clone()),
                        None => (name.clone(), vec![Tag::EndOfMibView.byte(), 0]),
                    };
                    if !push_varbind(&mut varbinds, &request, budget, &next, &value) {
                        trimmed = true;
                        break 'fill;
                    }
                    sent += 1;
                }
                let mut cursors: Vec<(Oid, bool)> = names[non_repeaters..]
                    .iter()
                    .map(|name| (name.clone(), false))
                    .collect();
                for _ in 0..max_repetitions {
                    if cursors.is_empty() || cursors.iter().all(|(_, end)| *end) {
                        break;
                    }
                    for (cursor, end) in cursors.iter_mut() {
                        let value = match (*end, agent.next_after(cursor)) {
                            (false, Some((next, value))) => {
                                *cursor = next.clone();
                                value.clone()
                            }
                            _ => {
                                *end = true;
                                vec![Tag::EndOfMibView.byte(), 0]
                            }
                        };
                        if !push_varbind(&mut varbinds, &request, budget, cursor, &value) {
                            trimmed = true;
                            break 'fill;
                        }
                        sent += 1;
                    }
                }
            }
            summary = format!(
                "GETBULK non-repeaters {} max-repetitions {} over {} varbind(s) -> {sent} varbind(s){}",
                request.error_status,
                request.error_index,
                names.len(),
                if trimmed { " (trimmed to budget)" } else { "" }
            );
        }
        Tag::SetRequest => {
            varbinds.extend_from_slice(request.varbinds);
            error = (NOT_WRITABLE, 1);
            summary = "SET -> notWritable".to_owned();
        }
        other => return Outcome::Dropped(format!("PDU {other:?} not answered")),
    }
    ber::write_message(
        out,
        request.version,
        request.community,
        Tag::Response,
        request.request_id,
        error.0,
        error.1,
        |out| out.extend_from_slice(&varbinds),
    );
    Outcome::Sent(format!(
        "request-id {} {summary}, reply {} bytes",
        request.request_id,
        out.len()
    ))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Bind and answer until killed.
pub fn run(args: impl Iterator<Item = String>) -> Result<(), String> {
    let args = parse_args(args)?;
    let socket = UdpSocket::bind(&args.listen).map_err(|e| format!("bind {}: {e}", args.listen))?;
    let local = socket.local_addr().map_err(|e| e.to_string())?;
    eprintln!(
        "snmp_spike listening on udp://{local}, budget {} bytes",
        args.budget
    );
    let mut agent = Agent::new();
    let mut inbound = vec![0u8; 65_535];
    let mut out = Vec::with_capacity(args.budget);
    loop {
        let (len, peer) = socket
            .recv_from(&mut inbound)
            .map_err(|e| format!("recv: {e}"))?;
        let datagram = &inbound[..len];
        if args.hex {
            eprintln!("<- {peer} {}", hex(datagram));
        }
        match answer(
            &mut agent,
            datagram,
            args.community.as_bytes(),
            args.budget,
            &mut out,
        ) {
            Outcome::Sent(line) => {
                if let Err(e) = socket.send_to(&out, peer) {
                    eprintln!("send to {peer}: {e}");
                }
                eprintln!("{peer} {line}");
                if args.hex {
                    eprintln!("-> {peer} {}", hex(&out));
                }
            }
            Outcome::Dropped(reason) => eprintln!("{peer} dropped: {reason}"),
        }
    }
}

#[allow(dead_code)]
fn main() {
    if let Err(e) = run(std::env::args().skip(1)) {
        eprintln!("snmp_spike: {e}");
        std::process::exit(1);
    }
}
