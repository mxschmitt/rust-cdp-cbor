//! Transport: spawn Chrome with `--remote-debugging-pipe=cbor` and exchange
//! crdtp-CBOR messages over the inherited pipe file descriptors.
//!
//! Upstream Chromium sources this mirrors:
//!   * The `--remote-debugging-pipe` switch (and `=cbor` value handling):
//!     content/public/common/content_switches.cc
//!     https://source.chromium.org/chromium/chromium/src/+/main:content/public/common/content_switches.cc
//!   * The pipe protocol itself (fd 3/4, framing, CBOR vs ASCIIZ mode):
//!     content/browser/devtools/devtools_pipe_handler.cc
//!     https://source.chromium.org/chromium/chromium/src/+/main:content/browser/devtools/devtools_pipe_handler.cc
//!
//! Chrome's pipe protocol:
//!   * The browser reads incoming commands from **fd 3** and writes outgoing
//!     messages to **fd 4** (from the browser's point of view).
//!   * In CBOR mode there is no NUL framing: each message is a self-delimiting
//!     crdtp envelope, so the reader peeks the 4-byte length and reads exactly
//!     that many bytes.
//!
//! We create two `pipe(2)` pairs and use `pre_exec` to dup them onto fd 3/4 in
//! the child before `exec`.

use crate::cbor;
use std::io::{Read, Write};
use std::os::unix::io::{FromRawFd, RawFd};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command};

pub struct PipeConn {
    child: Child,
    /// We write commands here; the child reads them from its fd 3.
    to_browser: std::fs::File,
    /// The child writes here (its fd 4); we read responses/events.
    from_browser: std::fs::File,
    buf: Vec<u8>,
}

fn make_pipe() -> std::io::Result<(RawFd, RawFd)> {
    let mut fds = [0 as RawFd; 2];
    // SAFETY: fds is a valid 2-element array for pipe(2).
    let rc = unsafe { libc::pipe(fds.as_mut_ptr()) };
    if rc != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok((fds[0], fds[1])) // (read end, write end)
}

impl PipeConn {
    /// Launch `chrome_path` headless with the CBOR debugging pipe wired to
    /// fd 3 (browser reads our commands) and fd 4 (browser writes to us).
    pub fn spawn(chrome_path: &str, extra_args: &[&str]) -> std::io::Result<Self> {
        // Pipe A: parent -> child stdin-side (browser's fd 3).
        let (a_read, a_write) = make_pipe()?;
        // Pipe B: child -> parent (browser's fd 4).
        let (b_read, b_write) = make_pipe()?;

        let mut cmd = Command::new(chrome_path);
        cmd.arg("--remote-debugging-pipe=cbor")
            .arg("--headless=new")
            .arg("--no-first-run")
            .arg("--no-default-browser-check")
            .arg("--user-data-dir=/tmp/rust-cdp-profile")
            .args(extra_args);

        // The child must inherit a_read as fd 3 (browser reads commands) and
        // b_write as fd 4 (browser writes responses).
        //
        // Ordering matters: pipe(2) hands out the lowest free fds, so a_write
        // is very likely fd 4 itself. We therefore close the parent-only ends
        // *first* to free those fd numbers, and only then dup2 into 3/4 — doing
        // it the other way round would clobber the output pipe.
        // SAFETY: dup2/close on raw fds inside the forked child before exec.
        unsafe {
            cmd.pre_exec(move || {
                // Drop the ends the child must not hold open (parent's sides).
                libc::close(a_write);
                libc::close(b_read);
                if libc::dup2(a_read, 3) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::dup2(b_write, 4) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                // Close the now-redundant source fds if they aren't already 3/4.
                if a_read != 3 && a_read != 4 {
                    libc::close(a_read);
                }
                if b_write != 3 && b_write != 4 {
                    libc::close(b_write);
                }
                Ok(())
            });
        }

        if std::env::var("CDP_DEBUG").is_err() {
            cmd.stderr(std::process::Stdio::null());
        }
        let child = cmd.spawn()?;

        // Parent closes the child-side ends.
        // SAFETY: these fds are owned by us and not yet wrapped.
        unsafe {
            libc::close(a_read);
            libc::close(b_write);
        }

        // SAFETY: a_write and b_read are valid fds we own; wrap for RAII.
        let to_browser = unsafe { std::fs::File::from_raw_fd(a_write) };
        let from_browser = unsafe { std::fs::File::from_raw_fd(b_read) };

        Ok(PipeConn {
            child,
            to_browser,
            from_browser,
            buf: Vec::with_capacity(64 * 1024),
        })
    }

    /// Send one already-encoded crdtp CBOR message.
    pub fn send_raw(&mut self, msg: &[u8]) -> std::io::Result<()> {
        self.to_browser.write_all(msg)?;
        self.to_browser.flush()
    }

    /// Read exactly one crdtp CBOR message frame, buffering any extra bytes.
    pub fn recv_raw(&mut self) -> std::io::Result<Vec<u8>> {
        let mut chunk = [0u8; 16 * 1024];
        loop {
            // Do we already have a complete frame buffered?
            if let Some(total) = cbor::message_len(&self.buf)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.0))?
            {
                if self.buf.len() >= total {
                    let frame = self.buf[..total].to_vec();
                    self.buf.drain(..total);
                    return Ok(frame);
                }
            }
            let n = self.from_browser.read(&mut chunk)?;
            if n == 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "browser closed pipe",
                ));
            }
            self.buf.extend_from_slice(&chunk[..n]);
        }
    }
}

impl Drop for PipeConn {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
