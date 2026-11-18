//! What the core needs from the app around it: a way to run tasks on its
//! thread, timers, and HTTP. The desktop browser gives it GLib's main loop
//! and libsoup; Android its own core thread.

use std::cell::RefCell;
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

pub type LocalFuture<T> = Pin<Box<dyn Future<Output = T>>>;

/// An HTTP answer: the status code and the body.
pub struct Response {
    pub status: u16,
    pub body: Vec<u8>,
}

pub trait Runtime {
    /// Run `task` on the core's thread, alongside what's running there.
    fn spawn_local(&self, task: LocalFuture<()>);
    /// Resolves after `delay`, without holding up the thread.
    fn sleep(&self, delay: Duration) -> LocalFuture<()>;
    /// An HTTP request; `body` is sent as `application/json`. Transport
    /// errors are `Err`, any status the server answered with is `Ok`.
    fn http(&self, method: &str, url: &str, body: Option<String>) -> LocalFuture<Result<Response, String>>;
}

thread_local! {
    static RUNTIME: RefCell<Option<Box<dyn Runtime>>> = const { RefCell::new(None) };
}

/// Called once by the host, on the core's thread, before anything else.
pub fn install(runtime: Box<dyn Runtime>) {
    RUNTIME.with(|r| *r.borrow_mut() = Some(runtime));
}

fn with<R>(f: impl FnOnce(&dyn Runtime) -> R) -> R {
    RUNTIME.with(|r| f(r.borrow().as_deref().expect("wisp-core: no runtime installed on this thread")))
}

pub fn spawn_local(task: impl Future<Output = ()> + 'static) {
    with(|r| r.spawn_local(Box::pin(task)));
}

pub fn sleep(delay: Duration) -> LocalFuture<()> {
    with(|r| r.sleep(delay))
}

pub fn http(method: &str, url: &str, body: Option<String>) -> LocalFuture<Result<Response, String>> {
    with(|r| r.http(method, url, body))
}

/// Run slow work (key derivation) on a thread of its own and wait for it
/// without holding up the core's thread.
pub async fn spawn_blocking<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> Result<T, String> {
    let (tx, rx) = futures_channel::oneshot::channel();
    std::thread::Builder::new()
        .name("wisp-work".into())
        .spawn(move || {
            let _ = tx.send(work());
        })
        .map_err(|e| e.to_string())?;
    rx.await.map_err(|_| "background work failed".to_string())
}
