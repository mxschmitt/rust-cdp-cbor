//! Minimal typed CDP client over Chrome's CBOR debugging pipe.
//!
//! Demonstrates the full path: spawn Chrome with `--remote-debugging-pipe=cbor`,
//! create a tab, attach to it, navigate, and read the page title — all using
//! strongly-typed `chromiumoxide_cdp` command structs encoded as crdtp CBOR.

mod cbor;
mod client;
mod pipe;

use chromiumoxide_cdp::cdp::browser_protocol::page::NavigateParams;
use chromiumoxide_cdp::cdp::browser_protocol::target::{
    AttachToTargetParams, CreateTargetParams,
};
use chromiumoxide_cdp::cdp::js_protocol::runtime::EvaluateParams;
use client::Client;
use pipe::PipeConn;

const DEFAULT_CHROME: &str =
    "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome";

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let url = args.next().unwrap_or_else(|| "https://example.com".to_string());
    let chrome = std::env::var("CHROME_PATH").unwrap_or_else(|_| DEFAULT_CHROME.to_string());

    eprintln!("launching: {chrome}");
    let conn = PipeConn::spawn(&chrome, &[])?;
    let mut client = Client::new(conn);

    // 1. Create a fresh tab (a "page" target).
    let created = client.execute(&CreateTargetParams::new("about:blank"), None)?;
    let target_id = created.target_id.clone();
    eprintln!("created target: {}", target_id.inner());

    // 2. Attach to it with flatten=true so we can drive it over a session id
    //    on the same pipe.
    let attach = AttachToTargetParams {
        target_id: target_id.clone(),
        flatten: Some(true),
    };
    let attached = client.execute(&attach, None)?;
    let session = attached.session_id.inner().clone();
    eprintln!("attached, session: {session}");

    // 3. Navigate the page to the requested URL.
    let nav = client.execute(&NavigateParams::new(url.clone()), Some(&session))?;
    eprintln!("navigated: frame {}", nav.frame_id.inner());

    // Give the page a moment to settle, then read the title. A real client
    // would wait for Page.loadEventFired; we poll document.readyState here to
    // stay minimal but reliable.
    wait_for_load(&mut client, &session)?;

    // 4. Read the page title via Runtime.evaluate, returned by value.
    let mut eval = EvaluateParams::new("document.title");
    eval.return_by_value = Some(true);
    let result = client.execute(&eval, Some(&session))?;

    let title = result
        .result
        .value
        .as_ref()
        .and_then(|v| v.as_str())
        .unwrap_or("<no title>");

    println!("URL:   {url}");
    println!("TITLE: {title}");
    Ok(())
}

/// Poll `document.readyState` until the page reports `complete` (bounded).
fn wait_for_load(client: &mut Client, session: &str) -> Result<(), Box<dyn std::error::Error>> {
    for _ in 0..100 {
        let mut eval = EvaluateParams::new("document.readyState");
        eval.return_by_value = Some(true);
        let r = client.execute(&eval, Some(session))?;
        if r.result.value.as_ref().and_then(|v| v.as_str()) == Some("complete") {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    Ok(())
}
