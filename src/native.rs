//! A plugin's native helper: a program from the plugin folder that Wisp
//! starts on demand and talks to over stdin/stdout, using the WebExtensions
//! native-messaging format (each message is JSON preceded by its length as a
//! 32-bit little-endian number), so helpers written for Firefox work as-is.

use gtk::prelude::*;
use gtk::{gio, glib};
use serde_json::Value as Json;
use std::ffi::OsString;
use std::path::Path;
use std::rc::Rc;
use std::time::Instant;

/// Firefox's limit for messages from a helper.
const MAX_MESSAGE: usize = 64 << 20;

pub struct NativePort {
    /// When it was started; tells a restarted helper from the old one.
    pub started: Instant,
    process: gio::Subprocess,
    stdin: gio::OutputStream,
}

impl NativePort {
    /// Start `argv` in `dir`. `on_message` gets each message; `on_exit` is
    /// called once when the helper exits or stops talking (with the reason).
    pub fn spawn(
        argv: &[OsString],
        dir: &Path,
        started: Instant,
        on_message: impl Fn(Json) + 'static,
        on_exit: impl FnOnce(String) + 'static,
    ) -> Result<Rc<NativePort>, String> {
        let launcher = gio::SubprocessLauncher::new(gio::SubprocessFlags::STDIN_PIPE | gio::SubprocessFlags::STDOUT_PIPE);
        launcher.set_cwd(dir);
        let args: Vec<&std::ffi::OsStr> = argv.iter().map(|a| a.as_os_str()).collect();
        let process = launcher.spawn(&args).map_err(|e| format!("cannot start the native helper: {e}"))?;
        let stdin = process.stdin_pipe().ok_or("the native helper has no input")?;
        let stdout = process.stdout_pipe().ok_or("the native helper has no output")?;
        let port = Rc::new(NativePort { started, process, stdin });
        glib::spawn_future_local(async move {
            let reason = loop {
                match read_message(&stdout).await {
                    Ok(Some(msg)) => on_message(msg),
                    Ok(None) => break "the native helper exited".to_string(),
                    Err(e) => break e,
                }
            };
            on_exit(reason);
        });
        Ok(port)
    }

    pub fn post(&self, msg: &Json) -> Result<(), String> {
        let body = msg.to_string().into_bytes();
        let mut frame = (body.len() as u32).to_le_bytes().to_vec();
        frame.extend_from_slice(&body);
        self.stdin
            .write_all(&frame, None::<&gio::Cancellable>)
            .map(|_| ())
            .map_err(|e| format!("cannot reach the native helper: {e}"))
    }

    /// Closing its input asks the helper to finish; it is killed if it hasn't
    /// after a few seconds (it may be finishing a download it was asked to cancel).
    pub fn close(&self) {
        let _ = self.stdin.close(None::<&gio::Cancellable>);
        let process = self.process.clone();
        glib::timeout_add_seconds_local_once(5, move || process.force_exit());
    }
}

async fn read_exact(stream: &gio::InputStream, len: usize) -> Result<Option<Vec<u8>>, String> {
    let (buf, read, _) = stream
        .read_all_future(vec![0u8; len], glib::Priority::DEFAULT)
        .await
        .map_err(|(_, e)| format!("reading from the native helper failed: {e}"))?;
    Ok((read == len).then_some(buf))
}

async fn read_message(stream: &gio::InputStream) -> Result<Option<Json>, String> {
    let Some(head) = read_exact(stream, 4).await? else { return Ok(None) };
    let len = u32::from_le_bytes([head[0], head[1], head[2], head[3]]) as usize;
    if len > MAX_MESSAGE {
        return Err("the native helper sent a message that is too large".into());
    }
    let Some(body) = read_exact(stream, len).await? else { return Ok(None) };
    serde_json::from_slice(&body).map(Some).map_err(|e| format!("the native helper sent invalid JSON: {e}"))
}
