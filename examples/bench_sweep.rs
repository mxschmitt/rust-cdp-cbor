//! CBOR-vs-JSON benchmark sweep — *when is CBOR worth it?*
//!
//! We sweep three payload **shapes** that correspond to real CDP responses,
//! scaling each up to ~5 MB, and measure four metrics per point:
//!   * wire size (bytes per ping/pong message)
//!   * encode time, decode time
//!   * round-trip throughput (encode+decode per second)
//!
//! Payload shapes (this is the axis that decides the winner):
//!   * `dom`     — `DOM.getDocument` tree: string-heavy (tag/attr names).
//!   * `numeric` — `Profiler`/`Performance`-style arrays of ints+floats.
//!   * `binary`  — `Page.captureScreenshot` blob: JSON must base64 it.
//!
//! Emits `bench/results.csv` and SVG charts under `bench/`.
//! Run: `cargo run --release --example bench_sweep`

use rust_cdp::cbor;
use serde::{Deserialize, Serialize};
use serde::de::DeserializeOwned;
use std::time::Instant;

// ---- payload types ---------------------------------------------------------

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
    attributes: Vec<String>,
    children: Vec<Node>,
}

fn build_dom(total: usize) -> Node {
    let mut counter = 0i32;
    build_node(total, 6, &mut counter)
}

fn build_node(remaining: usize, fanout: usize, counter: &mut i32) -> Node {
    *counter += 1;
    let id = *counter;
    let mut children = Vec::new();
    if remaining > 1 {
        let budget = remaining - 1;
        let per = (budget / fanout).max(1);
        let mut used = 0;
        for _ in 0..fanout {
            if used >= budget {
                break;
            }
            let take = per.min(budget - used);
            if take == 0 {
                break;
            }
            children.push(build_node(take, fanout, counter));
            used += take;
        }
    }
    Node {
        node_id: id,
        node_type: 1,
        node_name: "DIV".into(),
        local_name: "div".into(),
        node_value: String::new(),
        child_node_count: children.len() as i32,
        attributes: vec![
            "class".into(),
            "container flex items-center justify-between".into(),
            "data-id".into(),
            format!("node-{id}"),
            "role".into(),
            "presentation".into(),
        ],
        children,
    }
}

/// Numeric-heavy payload: coverage/profiler ranges (start,end,count) + floats.
#[derive(Serialize, Deserialize, Clone)]
struct NumericBlock {
    #[serde(rename = "scriptId")]
    script_id: i32,
    ranges: Vec<[i32; 3]>,
    samples: Vec<f64>,
}

fn build_numeric(count: usize) -> Vec<NumericBlock> {
    // ~count total triples spread across blocks of 256.
    let per = 256;
    let blocks = (count / per).max(1);
    (0..blocks)
        .map(|b| NumericBlock {
            script_id: b as i32,
            ranges: (0..per)
                .map(|i| {
                    let s = (i * 37) as i32;
                    [s, s + 100, (i % 13) as i32]
                })
                .collect(),
            samples: (0..per).map(|i| (i as f64) * 1.5 + 0.333).collect(),
        })
        .collect()
}

/// Binary payload: a screenshot-style blob carried as serde bytes.
#[derive(Serialize, Deserialize, Clone)]
struct Screenshot {
    format: String,
    width: i32,
    height: i32,
    #[serde(with = "serde_bytes")]
    data: Vec<u8>,
}

fn build_binary(bytes: usize) -> Screenshot {
    // Pseudo-random-ish but deterministic bytes (no Math.random in this env).
    let data: Vec<u8> = (0..bytes).map(|i| ((i * 2654435761usize) >> 13) as u8).collect();
    Screenshot {
        format: "png".into(),
        width: 1280,
        height: 720,
        data,
    }
}

// ---- measurement ------------------------------------------------------------

struct Point {
    shape: &'static str,
    nodes: usize,
    cbor_bytes: usize,
    json_bytes: usize,
    cbor_enc_us: f64,
    json_enc_us: f64,
    cbor_dec_us: f64,
    json_dec_us: f64,
    cbor_rt_per_s: f64,
    json_rt_per_s: f64,
}

/// Run `f` repeatedly for ~`secs`, return mean microseconds per call.
fn time_us<F: FnMut()>(secs: f64, mut f: F) -> f64 {
    f(); // warm up
    let mut iters = 0u64;
    let start = Instant::now();
    loop {
        f();
        iters += 1;
        let e = start.elapsed().as_secs_f64();
        if e >= secs {
            return e / iters as f64 * 1e6;
        }
    }
}

fn measure<T: Serialize + DeserializeOwned + Clone>(
    shape: &'static str,
    nodes: usize,
    v: &T,
) -> Point {
    let cbor = cbor::to_vec(v).expect("cbor encode");
    let json = serde_json::to_vec(v).expect("json encode");

    let secs = 0.35;
    let cbor_enc_us = time_us(secs, || {
        std::hint::black_box(cbor::to_vec(v).unwrap());
    });
    let json_enc_us = time_us(secs, || {
        std::hint::black_box(serde_json::to_vec(v).unwrap());
    });
    let cbor_dec_us = time_us(secs, || {
        let _: T = std::hint::black_box(cbor::from_slice(&cbor).unwrap());
    });
    let json_dec_us = time_us(secs, || {
        let _: T = std::hint::black_box(serde_json::from_slice(&json).unwrap());
    });

    Point {
        shape,
        nodes,
        cbor_bytes: cbor.len(),
        json_bytes: json.len(),
        cbor_enc_us,
        json_enc_us,
        cbor_dec_us,
        json_dec_us,
        cbor_rt_per_s: 1e6 / (cbor_enc_us + cbor_dec_us),
        json_rt_per_s: 1e6 / (json_enc_us + json_dec_us),
    }
}

fn main() -> std::io::Result<()> {
    let mut pts: Vec<Point> = Vec::new();

    println!("{:>8}  {:>8}  {:>10}  {:>10}  {:>7}  {:>10}  {:>10}",
        "shape", "n", "cbor(B)", "json(B)", "size%", "cbor rt/s", "json rt/s");

    // DOM sweep — node counts chosen so the top end lands near ~5 MB JSON.
    for &t in &[10usize, 50, 200, 1_000, 3_000, 8_000, 18_000, 32_000, 48_000, 64_000] {
        let dom = build_dom(t);
        let mut counter = 0;
        count_nodes(&dom, &mut counter);
        pts.push(measure("dom", counter, &dom));
        report(pts.last().unwrap());
    }
    // Numeric sweep.
    for &t in &[256usize, 1_024, 4_096, 16_384, 65_536, 200_000] {
        let num = build_numeric(t);
        pts.push(measure("numeric", t, &num));
        report(pts.last().unwrap());
    }
    // Binary sweep (raw byte counts up to ~5 MB).
    for &b in &[1_024usize, 16_384, 131_072, 1_048_576, 3_500_000] {
        let bin = build_binary(b);
        pts.push(measure("binary", b, &bin));
        report(pts.last().unwrap());
    }

    std::fs::create_dir_all("bench")?;
    write_csv(&pts)?;

    // Two headline charts over the DOM sweep (the "lots of DOM" question).
    let dom: Vec<&Point> = pts.iter().filter(|p| p.shape == "dom").collect();
    write_chart("bench/size.svg",
        "Wire size per message — CBOR vs JSON (DOM)",
        "DOM nodes (log)", "message size (KiB)",
        &[
            series("JSON", "#dc2626", &dom, |p| p.json_bytes as f64 / 1024.0),
            series("CBOR (crdtp)", "#2563eb", &dom, |p| p.cbor_bytes as f64 / 1024.0),
        ])?;
    write_chart_opt("bench/throughput.svg",
        "Throughput — encode+decode round-trips/sec (DOM, log-log)",
        "DOM nodes (log)", "round-trips / sec (log)",
        &[
            series("JSON", "#dc2626", &dom, |p| p.json_rt_per_s),
            series("CBOR (crdtp)", "#2563eb", &dom, |p| p.cbor_rt_per_s),
        ], true)?;

    // A "when is it worth it" chart: CBOR/JSON size ratio across all shapes.
    let bars: Vec<(String, f64, f64)> = pts.iter().map(|p| {
        (format!("{}:{}", p.shape, fmt_num(p.nodes as f64)),
         p.cbor_bytes as f64 / p.json_bytes as f64,
         p.cbor_rt_per_s / p.json_rt_per_s)
    }).collect();
    write_ratio_chart("bench/ratio.svg", &bars)?;

    println!("\nwrote bench/results.csv and bench/{{size,throughput,ratio}}.svg");
    Ok(())
}

fn count_nodes(n: &Node, c: &mut usize) {
    *c += 1;
    for ch in &n.children {
        count_nodes(ch, c);
    }
}

fn report(p: &Point) {
    let pct = (p.cbor_bytes as f64 / p.json_bytes as f64 - 1.0) * 100.0;
    println!("{:>8}  {:>8}  {:>10}  {:>10}  {:>+6.1}  {:>10.1}  {:>10.1}",
        p.shape, p.nodes, p.cbor_bytes, p.json_bytes, pct, p.cbor_rt_per_s, p.json_rt_per_s);
}

fn write_csv(pts: &[Point]) -> std::io::Result<()> {
    use std::fmt::Write;
    let mut s = String::new();
    writeln!(s, "shape,n,cbor_bytes,json_bytes,size_ratio,cbor_enc_us,json_enc_us,cbor_dec_us,json_dec_us,cbor_rt_per_s,json_rt_per_s").unwrap();
    for p in pts {
        writeln!(s, "{},{},{},{},{:.4},{:.2},{:.2},{:.2},{:.2},{:.1},{:.1}",
            p.shape, p.nodes, p.cbor_bytes, p.json_bytes,
            p.cbor_bytes as f64 / p.json_bytes as f64,
            p.cbor_enc_us, p.json_enc_us, p.cbor_dec_us, p.json_dec_us,
            p.cbor_rt_per_s, p.json_rt_per_s).unwrap();
    }
    std::fs::write("bench/results.csv", s)
}

// ---- tiny SVG plotter (raw strings use r##"..."## so #hex colors are ok) ----

struct Series {
    name: String,
    color: String,
    pts: Vec<(f64, f64)>,
}

fn series<F: Fn(&Point) -> f64>(name: &str, color: &str, pts: &[&Point], f: F) -> Series {
    Series {
        name: name.into(),
        color: color.into(),
        pts: pts.iter().map(|p| (p.nodes as f64, f(p))).collect(),
    }
}

fn write_chart(path: &str, title: &str, xl: &str, yl: &str, series: &[Series]) -> std::io::Result<()> {
    write_chart_opt(path, title, xl, yl, series, false)
}

fn write_chart_opt(path: &str, title: &str, xl: &str, yl: &str, series: &[Series], log_y: bool) -> std::io::Result<()> {
    let (w, h) = (840.0, 470.0);
    let (ml, mr, mt, mb) = (80.0, 175.0, 54.0, 66.0);
    let pw = w - ml - mr;
    let ph = h - mt - mb;
    let xs: Vec<f64> = series.iter().flat_map(|s| s.pts.iter().map(|p| p.0)).collect();
    let ys: Vec<f64> = series.iter().flat_map(|s| s.pts.iter().map(|p| p.1)).collect();
    let xmin = xs.iter().cloned().fold(f64::INFINITY, f64::min).max(1.0);
    let xmax = xs.iter().cloned().fold(0.0, f64::max);
    let ymax = ys.iter().cloned().fold(0.0, f64::max) * 1.08;
    let ymin_pos = ys.iter().cloned().filter(|v| *v > 0.0).fold(f64::INFINITY, f64::min);
    let (lxmin, lxmax) = (xmin.log10(), xmax.log10());
    let sx = |x: f64| ml + (x.max(1.0).log10() - lxmin) / (lxmax - lxmin) * pw;
    let (lymin, lymax) = (ymin_pos.log10().floor(), ymax.log10().ceil());
    let sy = |y: f64| {
        if log_y {
            mt + ph - (y.max(ymin_pos).log10() - lymin) / (lymax - lymin) * ph
        } else {
            mt + ph - (y / ymax) * ph
        }
    };

    let mut s = String::new();
    s.push_str(&format!(r##"<svg xmlns="http://www.w3.org/2000/svg" width="{w}" height="{h}" font-family="-apple-system,Segoe UI,Roboto,sans-serif"><rect width="{w}" height="{h}" fill="#ffffff"/>"##));
    s.push_str(&format!(r##"<text x="{ml}" y="30" font-size="18" font-weight="600" fill="#111">{}</text>"##, esc(title)));

    if log_y {
        let mut dy = lymin;
        while dy <= lymax + 1e-9 {
            let yv = 10f64.powf(dy);
            let y = sy(yv);
            s.push_str(&format!(r##"<line x1="{ml}" y1="{y:.1}" x2="{:.1}" y2="{y:.1}" stroke="#e5e7eb"/>"##, ml + pw));
            s.push_str(&format!(r##"<text x="{:.1}" y="{:.1}" font-size="11" fill="#6b7280" text-anchor="end">{}</text>"##, ml - 8.0, y + 4.0, fmt_num(yv)));
            dy += 1.0;
        }
    } else {
        for i in 0..=5 {
            let yv = ymax * i as f64 / 5.0;
            let y = sy(yv);
            s.push_str(&format!(r##"<line x1="{ml}" y1="{y:.1}" x2="{:.1}" y2="{y:.1}" stroke="#e5e7eb"/>"##, ml + pw));
            s.push_str(&format!(r##"<text x="{:.1}" y="{:.1}" font-size="11" fill="#6b7280" text-anchor="end">{}</text>"##, ml - 8.0, y + 4.0, fmt_num(yv)));
        }
    }
    let mut d = lxmin.floor();
    while d <= lxmax + 1e-9 {
        let xv = 10f64.powf(d);
        if xv >= xmin * 0.99 && xv <= xmax * 1.01 {
            let x = sx(xv);
            s.push_str(&format!(r##"<line x1="{x:.1}" y1="{mt}" x2="{x:.1}" y2="{:.1}" stroke="#f3f4f6"/>"##, mt + ph));
            s.push_str(&format!(r##"<text x="{x:.1}" y="{:.1}" font-size="11" fill="#6b7280" text-anchor="middle">{}</text>"##, mt + ph + 18.0, fmt_num(xv)));
        }
        d += 1.0;
    }
    s.push_str(&format!(r##"<line x1="{ml}" y1="{:.1}" x2="{:.1}" y2="{:.1}" stroke="#9ca3af" stroke-width="1.5"/>"##, mt + ph, ml + pw, mt + ph));
    s.push_str(&format!(r##"<line x1="{ml}" y1="{mt}" x2="{ml}" y2="{:.1}" stroke="#9ca3af" stroke-width="1.5"/>"##, mt + ph));
    s.push_str(&format!(r##"<text x="{:.1}" y="{:.1}" font-size="12" fill="#374151" text-anchor="middle">{}</text>"##, ml + pw / 2.0, h - 20.0, esc(xl)));
    s.push_str(&format!(r##"<text x="20" y="{:.1}" font-size="12" fill="#374151" text-anchor="middle" transform="rotate(-90 20 {:.1})">{}</text>"##, mt + ph / 2.0, mt + ph / 2.0, esc(yl)));

    for (i, ser) in series.iter().enumerate() {
        let mut path_d = String::new();
        for (j, &(x, y)) in ser.pts.iter().enumerate() {
            path_d.push_str(&format!("{}{:.1} {:.1}", if j == 0 { "M" } else { " L" }, sx(x), sy(y)));
        }
        s.push_str(&format!(r##"<path d="{path_d}" fill="none" stroke="{}" stroke-width="2.5"/>"##, ser.color));
        for &(x, y) in &ser.pts {
            s.push_str(&format!(r##"<circle cx="{:.1}" cy="{:.1}" r="3.2" fill="{}"/>"##, sx(x), sy(y), ser.color));
        }
        let ly = mt + 10.0 + i as f64 * 24.0;
        s.push_str(&format!(r##"<rect x="{:.1}" y="{:.1}" width="14" height="14" rx="3" fill="{}"/>"##, ml + pw + 24.0, ly, ser.color));
        s.push_str(&format!(r##"<text x="{:.1}" y="{:.1}" font-size="12" fill="#374151">{}</text>"##, ml + pw + 44.0, ly + 12.0, esc(&ser.name)));
    }
    s.push_str("</svg>");
    std::fs::write(path, s)
}

/// Grouped bar chart: size ratio and throughput ratio (CBOR/JSON) per payload.
/// A value < 1.0 on size means CBOR is smaller; > 1.0 on throughput means CBOR
/// is faster. The 1.0 line is the break-even marker.
fn write_ratio_chart(path: &str, bars: &[(String, f64, f64)]) -> std::io::Result<()> {
    let (w, h) = (1040.0, 500.0);
    let (ml, mr, mt, mb) = (70.0, 165.0, 64.0, 128.0);
    let pw = w - ml - mr;
    let ph = h - mt - mb;
    // Log-y axis: ratios span ~0.28 (binary size) to ~434 (binary speed), so a
    // linear axis hides the size bars near 1.0. Decades from 0.1 to 1000.
    let vmin = 0.1f64;
    let vmax = bars.iter().map(|b| b.1.max(b.2)).fold(1.0f64, f64::max) * 1.3;
    let (lmin, lmax) = (vmin.log10(), vmax.log10());
    let sy = |v: f64| mt + ph - (v.max(vmin).log10() - lmin) / (lmax - lmin) * ph;
    let n = bars.len();
    let group_w = pw / n as f64;
    let bw = group_w * 0.34;

    let mut s = String::new();
    s.push_str(&format!(r##"<svg xmlns="http://www.w3.org/2000/svg" width="{w}" height="{h}" font-family="-apple-system,Segoe UI,Roboto,sans-serif"><rect width="{w}" height="{h}" fill="#ffffff"/>"##));
    s.push_str(&format!(r##"<text x="{ml}" y="30" font-size="18" font-weight="600" fill="#111">When is CBOR worth it? — CBOR/JSON ratios per payload (log scale)</text>"##));
    s.push_str(&format!(r##"<text x="{ml}" y="48" font-size="12" fill="#6b7280">size ratio &lt; 1 = CBOR smaller · speed ratio &gt; 1 = CBOR faster · dashed line = break-even (1.0)</text>"##));

    // Log gridlines per decade.
    let mut d = lmin.floor();
    while d <= lmax + 1e-9 {
        let v = 10f64.powf(d);
        let y = sy(v);
        s.push_str(&format!(r##"<line x1="{ml}" y1="{y:.1}" x2="{:.1}" y2="{y:.1}" stroke="#eef2f7"/>"##, ml + pw));
        s.push_str(&format!(r##"<text x="{:.1}" y="{:.1}" font-size="11" fill="#6b7280" text-anchor="end">{}</text>"##, ml - 8.0, y + 4.0, fmt_ratio(v)));
        d += 1.0;
    }
    // break-even line at 1.0
    let y1 = sy(1.0);
    s.push_str(&format!(r##"<line x1="{ml}" y1="{y1:.1}" x2="{:.1}" y2="{y1:.1}" stroke="#111" stroke-dasharray="5 4" stroke-width="1.4"/>"##, ml + pw));
    let base = sy(vmin); // bars grow from the bottom of the plot

    for (i, (label, size_ratio, speed_ratio)) in bars.iter().enumerate() {
        let gx = ml + i as f64 * group_w + group_w / 2.0;
        // size ratio bar (left, blue)
        let x1 = gx - bw - 2.0;
        let y = sy(*size_ratio);
        s.push_str(&format!(r##"<rect x="{x1:.1}" y="{y:.1}" width="{bw:.1}" height="{:.1}" fill="#2563eb"/>"##, base - y));
        // speed ratio bar (right, green)
        let x2 = gx + 2.0;
        let y = sy(*speed_ratio);
        s.push_str(&format!(r##"<rect x="{x2:.1}" y="{y:.1}" width="{bw:.1}" height="{:.1}" fill="#16a34a"/>"##, base - y));
        // x label rotated
        s.push_str(&format!(r##"<text x="{gx:.1}" y="{:.1}" font-size="10" fill="#374151" text-anchor="end" transform="rotate(-40 {gx:.1} {:.1})">{}</text>"##, mt + ph + 14.0, mt + ph + 14.0, esc(label)));
    }
    // legend
    s.push_str(&format!(r##"<rect x="{:.1}" y="{mt}" width="14" height="14" rx="3" fill="#2563eb"/><text x="{:.1}" y="{:.1}" font-size="12" fill="#374151">size  (cbor/json)</text>"##, ml + pw + 22.0, ml + pw + 42.0, mt + 12.0));
    s.push_str(&format!(r##"<rect x="{:.1}" y="{:.1}" width="14" height="14" rx="3" fill="#16a34a"/><text x="{:.1}" y="{:.1}" font-size="12" fill="#374151">speed (cbor/json)</text>"##, ml + pw + 22.0, mt + 24.0, ml + pw + 42.0, mt + 36.0));
    s.push_str("</svg>");
    std::fs::write(path, s)
}

fn fmt_ratio(v: f64) -> String {
    if v >= 1.0 {
        format!("{v:.0}x")
    } else {
        format!("{v:.1}")
    }
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

fn fmt_num(v: f64) -> String {
    if v >= 1_000_000.0 {
        format!("{:.1}M", v / 1_000_000.0)
    } else if v >= 1_000.0 {
        format!("{:.0}k", v / 1_000.0)
    } else {
        format!("{v:.0}")
    }
}
