//! Where does the time go? — a profiling harness for the crdtp CBOR codec.
//!
//! Rather than guess, we isolate the codec's sub-costs with synthetic payloads
//! that stress one dimension each (ints, short strings, nesting, bytes), plus a
//! realistic DOM payload, and print a per-operation breakdown. We then A/B a
//! few candidate optimizations against the same payloads.
//!
//! Run: `cargo run --release --example profile`

use rust_cdp::cbor;
use serde::{Deserialize, Serialize};
use std::hint::black_box;
use std::time::{Duration, Instant};

/// Run `f` for ~`secs`, return (mean per-call duration, calls/sec).
fn bench<F: FnMut()>(secs: f64, mut f: F) -> (Duration, f64) {
    f(); // warm up
    let mut iters = 0u64;
    let start = Instant::now();
    loop {
        f();
        iters += 1;
        let e = start.elapsed().as_secs_f64();
        if e >= secs {
            return (Duration::from_secs_f64(e / iters as f64), iters as f64 / e);
        }
    }
}

fn mb_per_s(bytes: usize, per_call: Duration) -> f64 {
    bytes as f64 / per_call.as_secs_f64() / 1e6
}

// ---- payloads that stress one dimension each ------------------------------

#[derive(Serialize, Deserialize, Clone)]
struct Ints {
    v: Vec<i32>,
}
#[derive(Serialize, Deserialize, Clone)]
struct Strs {
    v: Vec<String>,
}
#[derive(Serialize, Deserialize, Clone)]
struct Node {
    #[serde(rename = "nodeId")]
    node_id: i32,
    #[serde(rename = "nodeName")]
    node_name: String,
    attributes: Vec<String>,
    children: Vec<Node>,
}
fn dom(depth: u32, fanout: u32, c: &mut i32) -> Node {
    *c += 1;
    let id = *c;
    let children = if depth == 0 {
        vec![]
    } else {
        (0..fanout).map(|_| dom(depth - 1, fanout, c)).collect()
    };
    Node {
        node_id: id,
        node_name: "DIV".into(),
        attributes: vec!["class".into(), "container flex".into(), "id".into(), format!("n{id}")],
        children,
    }
}

fn main() {
    let secs = 0.4;
    println!("=== component breakdown (encode + decode throughput) ===\n");

    // 100k ints — stresses integer token writing/reading.
    let ints = Ints { v: (0..100_000).map(|i| (i * 7 - 3) as i32).collect() };
    profile("100k i32", secs, &ints);

    // 50k short ASCII strings — stresses string length tokens + utf8 validation.
    let strs = Strs { v: (0..50_000).map(|i| format!("attr-{i}")).collect() };
    profile("50k short strings", secs, &strs);

    // DOM tree (~9k nodes) — realistic mix.
    let mut c = 0;
    let tree = dom(5, 6, &mut c);
    profile(&format!("DOM {c} nodes"), secs, &tree);

    println!("\n=== where in DECODE the time goes (DOM) ===\n");
    decode_breakdown(secs, &tree);

    println!("\n=== where in DECODE the time goes (100k ints) ===\n");
    decode_breakdown(secs, &ints);

    println!("\n=== where in DECODE the time goes (50k strings) ===\n");
    decode_breakdown(secs, &strs);

    println!("\n=== encode: fresh-alloc vs reused buffer (DOM) ===\n");
    encode_buffer_ab(secs, &tree);
}

/// A/B the cost of allocating a fresh output Vec per encode vs reusing one.
fn encode_buffer_ab<T: Serialize>(secs: f64, v: &T) {
    let n = cbor::to_vec(v).unwrap().len();
    let (fresh, _) = bench(secs, || {
        black_box(cbor::to_vec(black_box(v)).unwrap());
    });
    let mut buf = Vec::with_capacity(n + 64);
    let (reused, _) = bench(secs, || {
        buf.clear();
        cbor::to_buf(black_box(v), &mut buf).unwrap();
        black_box(&buf);
    });
    println!("fresh Vec per call : {:>8.1} MB/s ({:?})", mb_per_s(n, fresh), fresh);
    println!("reused buffer      : {:>8.1} MB/s ({:?})", mb_per_s(n, reused), reused);
    let gain = (reused.as_secs_f64() / fresh.as_secs_f64() - 1.0) * -100.0;
    println!("  -> reuse saves {gain:.1}% of encode time");
}

fn profile<T: Serialize + for<'de> Deserialize<'de>>(name: &str, secs: f64, v: &T) {
    let bytes = cbor::to_vec(v).unwrap();
    let json = serde_json::to_vec(v).unwrap();
    let n = bytes.len();
    let (enc, _) = bench(secs, || {
        black_box(cbor::to_vec(black_box(v)).unwrap());
    });
    let (dec, _) = bench(secs, || {
        let _: T = black_box(cbor::from_slice(black_box(&bytes)).unwrap());
    });
    // Same payload through serde_json, as a reference point.
    let (jenc, _) = bench(secs, || {
        black_box(serde_json::to_vec(black_box(v)).unwrap());
    });
    let (jdec, _) = bench(secs, || {
        let _: T = black_box(serde_json::from_slice(black_box(&json)).unwrap());
    });
    println!(
        "{name:<18} {n:>8}B  | CBOR enc {:>7.0} dec {:>7.0} MB/s | JSON enc {:>7.0} dec {:>7.0} MB/s",
        mb_per_s(n, enc),
        mb_per_s(n, dec),
        mb_per_s(json.len(), jenc),
        mb_per_s(json.len(), jdec),
    );
}

/// Break decode into: full typed decode vs decode-into-IgnoredAny (pure parse,
/// no struct building) vs raw byte scan. The gaps attribute time to parsing,
/// visitor/struct construction, and memory traffic respectively.
fn decode_breakdown<T: Serialize + for<'de> Deserialize<'de>>(secs: f64, v: &T) {
    let bytes = cbor::to_vec(v).unwrap();
    let n = bytes.len();

    let (full, _) = bench(secs, || {
        let _: T = black_box(cbor::from_slice(black_box(&bytes)).unwrap());
    });
    let (ignored, _) = bench(secs, || {
        let _: serde::de::IgnoredAny = black_box(cbor::from_slice(black_box(&bytes)).unwrap());
    });
    let (scan, _) = bench(secs, || {
        // Baseline: just touch every byte once (memory bandwidth floor).
        let mut sum = 0u8;
        for &b in black_box(&bytes) {
            sum = sum.wrapping_add(b);
        }
        black_box(sum);
    });

    println!("full typed decode : {:>8.1} MB/s ({:?})", mb_per_s(n, full), full);
    println!("parse-only (Ignored): {:>8.1} MB/s ({:?})", mb_per_s(n, ignored), ignored);
    println!("byte-scan baseline : {:>8.1} MB/s ({:?})", mb_per_s(n, scan), scan);
    let parse_frac = ignored.as_secs_f64() / full.as_secs_f64() * 100.0;
    println!(
        "\n  -> parsing is {:.0}% of full decode; struct-building is the other {:.0}%",
        parse_frac,
        100.0 - parse_frac
    );
}
