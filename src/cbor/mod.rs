//! A serde codec for Chrome DevTools' CBOR dialect ("crdtp" CBOR).
//!
//! Chrome's `--remote-debugging-pipe=cbor` mode exchanges messages encoded
//! with the inspector-protocol CBOR variant, which differs from RFC 7049 CBOR
//! in a few deliberate ways (enveloped + indefinite-length maps/arrays,
//! int32-range scalars, UTF-8/UTF-16 strings). Off-the-shelf CBOR crates
//! cannot read or write it, so this module implements the dialect directly on
//! top of serde — letting any serde-derived CDP type round-trip unchanged.

pub mod consts;
pub mod de;
pub mod ser;

pub use de::{from_slice, message_len};
pub use ser::{to_buf, to_vec};

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct Navigate {
        url: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        referrer: Option<String>,
        transition_count: i32,
        active: bool,
        ratio: f64,
        frames: Vec<String>,
    }

    #[test]
    fn round_trip() {
        let n = Navigate {
            url: "https://example.com".into(),
            referrer: None,
            transition_count: -7,
            active: true,
            ratio: 1.5,
            frames: vec!["a".into(), "b".into()],
        };
        let bytes = to_vec(&n).unwrap();
        // Top-level message must look like a crdtp envelope.
        assert!(consts::is_cbor_message(&bytes), "bytes: {bytes:02x?}");
        // The declared frame length must cover exactly the buffer.
        assert_eq!(message_len(&bytes).unwrap(), Some(bytes.len()));
        let back: Navigate = from_slice(&bytes).unwrap();
        assert_eq!(n, back);
    }

    #[test]
    fn envelope_bytes() {
        // A minimal {} map should be: D8 18 5A 00000001 BF FF
        #[derive(Serialize)]
        struct Empty {}
        let bytes = to_vec(&Empty {}).unwrap();
        assert_eq!(&bytes[..3], &[0xD8, 0x18, 0x5A]);
        let len = u32::from_be_bytes(bytes[3..7].try_into().unwrap());
        assert_eq!(len, 2); // BF FF
        assert_eq!(&bytes[7..], &[0xBF, 0xFF]);
    }

    /// Compare our crdtp-CBOR codec against serde_json on a realistic CDP
    /// request, for both size and encode/decode throughput. Run with:
    ///   cargo test --release -- --nocapture bench_cbor_vs_json
    #[test]
    fn bench_cbor_vs_json() {
        #[derive(Serialize, Deserialize, Clone)]
        struct Eval {
            expression: String,
            #[serde(rename = "returnByValue")]
            return_by_value: bool,
            #[serde(rename = "awaitPromise")]
            await_promise: bool,
        }
        #[derive(Serialize, Deserialize, Clone)]
        struct Req {
            id: u64,
            method: String,
            params: Eval,
            #[serde(rename = "sessionId")]
            session_id: String,
        }
        let req = Req {
            id: 42,
            method: "Runtime.evaluate".into(),
            params: Eval {
                expression: "document.title".into(),
                return_by_value: true,
                await_promise: false,
            },
            session_id: "A5FF6DD5F68E134D97EEB6044B76873D".into(),
        };

        let cbor = to_vec(&req).unwrap();
        let json = serde_json::to_vec(&req).unwrap();
        eprintln!("\n  size:  cbor = {} bytes, json = {} bytes", cbor.len(), json.len());

        const N: u32 = 200_000;
        let t = std::time::Instant::now();
        for i in 0..N {
            let mut r = req.clone();
            r.id = i as u64;
            std::hint::black_box(to_vec(&r).unwrap());
        }
        let cbor_enc = t.elapsed();
        let t = std::time::Instant::now();
        for i in 0..N {
            let mut r = req.clone();
            r.id = i as u64;
            std::hint::black_box(serde_json::to_vec(&r).unwrap());
        }
        let json_enc = t.elapsed();

        let t = std::time::Instant::now();
        for _ in 0..N {
            let _: serde_json::Value = std::hint::black_box(from_slice(&cbor).unwrap());
        }
        let cbor_dec = t.elapsed();
        let t = std::time::Instant::now();
        for _ in 0..N {
            let _: serde_json::Value =
                std::hint::black_box(serde_json::from_slice(&json).unwrap());
        }
        let json_dec = t.elapsed();

        eprintln!("  encode {N} iters: cbor = {cbor_enc:?}, json = {json_enc:?}");
        eprintln!("  decode {N} iters: cbor = {cbor_dec:?}, json = {json_dec:?}\n");
    }

    /// Simulate a large `DOM.getDocument` style response: a deep, wide tree of
    /// nodes mixing small ints (nodeId/nodeType), short strings (tag names),
    /// and string arrays (attributes). This is the payload shape that matters
    /// when "the browser returns a lot of DOM".
    ///
    ///   cargo test --release -- --nocapture bench_large_dom
    #[test]
    fn bench_large_dom() {
        #[derive(Serialize, Deserialize, Clone)]
        struct Node {
            #[serde(rename = "nodeId")]
            node_id: i32,
            #[serde(rename = "nodeType")]
            node_type: i32,
            #[serde(rename = "nodeName")]
            node_name: String,
            #[serde(rename = "localName")]
            local_name: String,
            #[serde(rename = "nodeValue")]
            node_value: String,
            #[serde(rename = "childNodeCount")]
            child_node_count: i32,
            // attributes are a flat [name, value, name, value, ...] string list
            attributes: Vec<String>,
            children: Vec<Node>,
        }

        // Build a tree: `fanout` children per node, `depth` deep.
        fn build(depth: u32, fanout: u32, counter: &mut i32) -> Node {
            *counter += 1;
            let id = *counter;
            let children = if depth == 0 {
                Vec::new()
            } else {
                (0..fanout).map(|_| build(depth - 1, fanout, counter)).collect()
            };
            Node {
                node_id: id,
                node_type: 1,
                node_name: "DIV".into(),
                local_name: "div".into(),
                node_value: String::new(),
                child_node_count: children.len() as i32,
                attributes: vec![
                    "class".into(),
                    "container flex items-center".into(),
                    "data-id".into(),
                    format!("node-{id}"),
                    "role".into(),
                    "presentation".into(),
                ],
                children,
            }
        }

        let mut counter = 0;
        let root = build(6, 6, &mut counter); // ~55k nodes
        eprintln!("\n  large DOM: {} nodes", counter);

        let cbor = to_vec(&root).unwrap();
        let json = serde_json::to_vec(&root).unwrap();
        eprintln!(
            "  size:  cbor = {} KiB, json = {} KiB  ({:+.1}% vs json)",
            cbor.len() / 1024,
            json.len() / 1024,
            (cbor.len() as f64 / json.len() as f64 - 1.0) * 100.0
        );

        const N: u32 = 200;
        let t = std::time::Instant::now();
        for _ in 0..N {
            std::hint::black_box(to_vec(&root).unwrap());
        }
        let cbor_enc = t.elapsed() / N;
        let t = std::time::Instant::now();
        for _ in 0..N {
            std::hint::black_box(serde_json::to_vec(&root).unwrap());
        }
        let json_enc = t.elapsed() / N;

        let t = std::time::Instant::now();
        for _ in 0..N {
            let _: Node = std::hint::black_box(from_slice(&cbor).unwrap());
        }
        let cbor_dec = t.elapsed() / N;
        let t = std::time::Instant::now();
        for _ in 0..N {
            let _: Node = std::hint::black_box(serde_json::from_slice(&json).unwrap());
        }
        let json_dec = t.elapsed() / N;

        eprintln!("  encode/msg: cbor = {cbor_enc:?}, json = {json_enc:?}");
        eprintln!("  decode/msg: cbor = {cbor_dec:?}, json = {json_dec:?}\n");
    }

    /// Quantify the client's single-pass decode win: decoding CBOR straight
    /// into a typed struct vs the old two-pass route (CBOR -> serde_json::Value
    /// -> typed via from_value).
    ///
    ///   cargo test --release -- --nocapture bench_decode_paths
    #[test]
    fn bench_decode_paths() {
        #[derive(Serialize, Deserialize, Clone)]
        struct Node {
            #[serde(rename = "nodeId")]
            node_id: i32,
            #[serde(rename = "nodeName")]
            node_name: String,
            attributes: Vec<String>,
            children: Vec<Node>,
        }
        fn build(depth: u32, fanout: u32, c: &mut i32) -> Node {
            *c += 1;
            let id = *c;
            let children = if depth == 0 {
                vec![]
            } else {
                (0..fanout).map(|_| build(depth - 1, fanout, c)).collect()
            };
            Node {
                node_id: id,
                node_name: "DIV".into(),
                attributes: vec!["class".into(), "container".into(), "id".into(), format!("n{id}")],
                children,
            }
        }
        let mut c = 0;
        let root = build(5, 6, &mut c);
        let cbor = to_vec(&root).unwrap();
        eprintln!("\n  decode paths: {} nodes, {} KiB cbor", c, cbor.len() / 1024);

        const N: u32 = 2000;
        let t = std::time::Instant::now();
        for _ in 0..N {
            let _: Node = std::hint::black_box(from_slice(&cbor).unwrap());
        }
        let direct = t.elapsed() / N;

        let t = std::time::Instant::now();
        for _ in 0..N {
            let v: serde_json::Value = from_slice(&cbor).unwrap();
            let _: Node = std::hint::black_box(serde_json::from_value(v).unwrap());
        }
        let two_pass = t.elapsed() / N;

        let speedup = two_pass.as_secs_f64() / direct.as_secs_f64();
        eprintln!("  direct (cbor->typed):     {direct:?}");
        eprintln!("  two-pass (cbor->Value->typed): {two_pass:?}");
        eprintln!("  -> single-pass is {speedup:.2}x faster\n");
    }
}
