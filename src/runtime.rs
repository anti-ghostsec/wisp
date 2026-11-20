//! wisp-core's runtime on the desktop: GLib's main loop and libsoup.

use std::time::Duration;
use webkit6::glib;
use webkit6::glib::translate::IntoGlib;
use webkit6::soup;
use webkit6::soup::prelude::*;
use wisp_core::rt::{LocalFuture, Response, Runtime};

pub struct Glib {
    http: soup::Session,
}

impl Glib {
    pub fn new() -> Glib {
        let http = soup::Session::new();
        http.set_timeout(20);
        http.set_user_agent("wisp");
        Glib { http }
    }
}

impl Runtime for Glib {
    fn spawn_local(&self, task: LocalFuture<()>) {
        glib::spawn_future_local(task);
    }

    fn sleep(&self, delay: Duration) -> LocalFuture<()> {
        Box::pin(glib::timeout_future(delay))
    }

    fn http(&self, method: &str, url: &str, body: Option<String>) -> LocalFuture<Result<Response, String>> {
        let msg = soup::Message::new(method, url).map_err(|e| format!("bad URL {url}: {e}"));
        let http = self.http.clone();
        Box::pin(async move {
            let msg = msg?;
            // Never sent on elsewhere: the core sees a redirect, and refuses it.
            msg.add_flags(soup::MessageFlags::NO_REDIRECT);
            if let Some(body) = body {
                let bytes = glib::Bytes::from_owned(body.into_bytes());
                msg.set_request_body_from_bytes(Some("application/json"), Some(&bytes));
            }
            let bytes = http.send_and_read_future(&msg, glib::Priority::DEFAULT).await.map_err(|e| e.to_string())?;
            Ok(Response { status: msg.status().into_glib() as u16, body: bytes.to_vec() })
        })
    }
}
