//! Sharing to other programs on FerroOS, through its Share box
//! (`ferro-share`): the user picks the program there, and it gets a copy in
//! memory. Wisp isn't told which program, or whether anything was shared;
//! where there's no Share box (not FerroOS) there's no Share in Wisp either.

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use webkit6::glib;
use webkit6::glib::translate::IntoGlib;
use webkit6::soup;
use webkit6::soup::prelude::*;

/// FerroOS's Share box, by its full path: not whatever is first on a PATH.
const BIN: &str = "/usr/local/bin/ferro-share";
/// The most a shared picture may weigh (the Share box holds it in memory).
const MAX_PICTURE: usize = 64 * 1024 * 1024;

/// The Share box to run: FerroOS's, or a stand-in the tests name.
fn bin() -> String {
    #[cfg(test)]
    if let Ok(b) = std::env::var("WISP_TEST_SHARE_BIN") {
        return b;
    }
    BIN.into()
}

/// FerroOS's Share box is here.
pub fn available() -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(bin()).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// Share a link (a page, or a link on one).
pub fn link(url: &str) -> Result<(), String> {
    let mut cmd = Command::new(bin());
    cmd.args(["--url", url]);
    spawn(cmd, None)
}

/// Share `data` as a file named `name`; it goes to the Share box through a
/// pipe, never through a file.
pub fn bytes(name: &str, data: &[u8]) -> Result<(), String> {
    let mut cmd = Command::new(bin());
    cmd.args(["--stdin", name]);
    spawn(cmd, Some(data))
}

fn spawn(mut cmd: Command, input: Option<&[u8]>) -> Result<(), String> {
    cmd.stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() }).stdout(Stdio::null());
    let mut child = cmd.spawn().map_err(|e| format!("couldn't open the Share box: {e}"))?;
    let data = input.map(<[u8]>::to_vec);
    let stdin = child.stdin.take();
    // Written and waited for aside: a big picture doesn't hold up the
    // window, and the box isn't left a zombie.
    std::thread::spawn(move || {
        if let (Some(data), Some(mut stdin)) = (data, stdin) {
            let _ = stdin.write_all(&data);
        }
        let _ = child.wait();
    });
    Ok(())
}

/// Share the picture at `uri`, shown on the page at `page`: fetched again
/// into memory (in a session of its own, with no cookies, telling the
/// picture's server only which site it's on), or taken from a `data:`
/// address as it is.
pub async fn picture(uri: &str, page: &str) -> Result<(), String> {
    let (data, mime) = if let Some(rest) = uri.strip_prefix("data:") {
        from_data_uri(rest)?
    } else if uri.starts_with("https://") || uri.starts_with("http://") {
        fetch(uri, page).await?
    } else {
        return Err("this picture can't be shared (it only exists inside the page)".into());
    };
    if data.len() > MAX_PICTURE {
        return Err(format!("the picture is larger than {} MB", MAX_PICTURE / (1024 * 1024)));
    }
    bytes(&picture_name(uri, mime.as_deref()), &data)
}

async fn fetch(uri: &str, page: &str) -> Result<(Vec<u8>, Option<String>), String> {
    let session = soup::Session::new();
    session.set_timeout(30);
    session.set_user_agent("Mozilla/5.0");
    let msg = soup::Message::new("GET", uri).map_err(|e| format!("bad address: {e}"))?;
    if let Some(origin) = origin(page)
        && let Some(headers) = msg.request_headers()
    {
        // Some sites only serve their pictures to their own pages.
        headers.append("Referer", &format!("{origin}/"));
    }
    let body = session
        .send_and_read_future(&msg, glib::Priority::DEFAULT)
        .await
        .map_err(|e| format!("couldn't fetch the picture: {e}"))?;
    let status = msg.status().into_glib();
    if !(200..300).contains(&status) {
        return Err(format!("the picture's server answered {status}"));
    }
    let mime = msg
        .response_headers()
        .and_then(|h| h.content_type().map(|(t, _)| t.to_string()));
    Ok((body.to_vec(), mime))
}

/// `scheme://host[:port]` of a page, for the Referer.
fn origin(page: &str) -> Option<String> {
    let (scheme, rest) = page.split_once("://")?;
    if scheme != "https" && scheme != "http" {
        return None;
    }
    let host = rest.split(['/', '?', '#']).next()?;
    let host = host.rsplit('@').next()?;
    (!host.is_empty()).then(|| format!("{scheme}://{host}"))
}

/// `image/png;base64,....` (or not base64) to its bytes and type.
fn from_data_uri(rest: &str) -> Result<(Vec<u8>, Option<String>), String> {
    let (meta, payload) = rest.split_once(',').ok_or("this picture's address is damaged")?;
    let base64 = meta.ends_with(";base64");
    let mime = meta.trim_end_matches(";base64").split(';').next().filter(|m| !m.is_empty()).map(str::to_owned);
    let data = if base64 {
        B64.decode(payload.trim()).map_err(|_| "this picture's address is damaged")?
    } else {
        percent_decode(payload)
    };
    Ok((data, mime))
}

fn percent_decode(s: &str) -> Vec<u8> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let Ok(v) = u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or(""), 16)
        {
            out.push(v);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    out
}

/// A name for the shared picture: the last part of its address, with an
/// extension that says what it is (the Share box goes by it).
fn picture_name(uri: &str, mime: Option<&str>) -> String {
    let ext = match mime.unwrap_or("") {
        "image/jpeg" | "image/jpg" => "jpg",
        "image/png" => "png",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/avif" => "avif",
        "image/svg+xml" => "svg",
        "image/bmp" => "bmp",
        _ => "",
    };
    let last = if uri.starts_with("data:") {
        ""
    } else {
        uri.split(['?', '#']).next().unwrap_or("").rsplit('/').next().unwrap_or("")
    };
    let last = String::from_utf8_lossy(&percent_decode(last)).into_owned();
    let stem: String = last.chars().filter(|c| !matches!(c, '/' | '\\' | '\0')).take(100).collect();
    let stem = stem.trim().trim_start_matches('.');
    let has_ext = Path::new(stem).extension().is_some_and(|e| !e.is_empty());
    match (stem.is_empty(), has_ext, ext.is_empty()) {
        (true, _, true) => "picture".into(),
        (true, _, false) => format!("picture.{ext}"),
        (false, true, _) => stem.to_owned(),
        (false, false, true) => stem.to_owned(),
        (false, false, false) => format!("{stem}.{ext}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picture_names_say_what_they_are() {
        assert_eq!(picture_name("https://a.b/img/cat.jpg?w=200", Some("image/jpeg")), "cat.jpg");
        assert_eq!(picture_name("https://a.b/img/cat", Some("image/png")), "cat.png");
        assert_eq!(picture_name("https://a.b/", Some("image/webp")), "picture.webp");
        assert_eq!(picture_name("data:image/gif;base64,R0lG", Some("image/gif")), "picture.gif");
        assert_eq!(picture_name("https://a.b/my%20cat.png", None), "my cat.png");
        assert_eq!(picture_name("https://a.b/..hidden", None), "hidden");
    }

    #[test]
    fn data_addresses_are_read() {
        let (d, m) = from_data_uri("image/png;base64,iVBORw0K").unwrap();
        assert_eq!(m.as_deref(), Some("image/png"));
        assert_eq!(&d[1..4], b"PNG");
        let (d, m) = from_data_uri("image/svg+xml,%3Csvg%3E").unwrap();
        assert_eq!(m.as_deref(), Some("image/svg+xml"));
        assert_eq!(d, b"<svg>");
        assert!(from_data_uri("nocomma").is_err());
    }

    #[test]
    fn origins() {
        assert_eq!(origin("https://u:p@www.ex.com:8443/a?b").as_deref(), Some("https://www.ex.com:8443"));
        assert_eq!(origin("about:blank"), None);
        assert_eq!(origin("file:///x"), None);
    }

    /// The whole way from a picture's address to the Share box: fetched from
    /// a server (the page's site sent as Referer, no cookies), or read from
    /// a `data:` address, and piped in; links passed as they are.
    #[test]
    fn pictures_and_links_reach_the_share_box() {
        use std::io::{BufRead, BufReader, Read};
        use std::net::TcpListener;
        let dir = std::env::temp_dir().join(format!("wisp-share-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join("got");
        let fake = dir.join("ferro-share");
        std::fs::write(&fake, format!("#!/bin/sh\nprintf '%s\\n' \"$@\" > '{0}.args'\ncat > '{0}.data'\n", out.display())).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        // SAFETY: the only test that sets it; read in `bin()`.
        unsafe { std::env::set_var("WISP_TEST_SHARE_BIN", &fake) };
        assert!(available());

        let png: &[u8] = b"\x89PNG\r\n\x1a\nfake picture";
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let mut head = String::new();
            let mut r = BufReader::new(s.try_clone().unwrap());
            loop {
                let mut line = String::new();
                r.read_line(&mut line).unwrap();
                if line == "\r\n" || line.is_empty() { break }
                head.push_str(&line);
            }
            write!(s, "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", png.len()).unwrap();
            s.write_all(png).unwrap();
            head
        });
        let wait = |name: &str| {
            for _ in 0..100 {
                if std::fs::metadata(dir.join(name)).is_ok_and(|m| m.len() > 0) { return }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            panic!("the stand-in Share box never got {name}");
        };
        let read = |name: &str| {
            let mut v = Vec::new();
            std::fs::File::open(dir.join(name)).unwrap().read_to_end(&mut v).unwrap();
            let _ = std::fs::remove_file(dir.join(name));
            v
        };

        glib::MainContext::default()
            .block_on(picture(&format!("http://127.0.0.1:{port}/photos/cat"), "https://news.example.com/story?id=1"))
            .unwrap();
        let head = server.join().unwrap();
        assert!(head.contains("Referer: https://news.example.com/\r\n"), "{head}");
        assert!(!head.to_ascii_lowercase().contains("cookie"), "{head}");
        wait("got.data");
        assert_eq!(read("got.data"), png);
        assert_eq!(String::from_utf8(read("got.args")).unwrap(), "--stdin\ncat.png\n");

        glib::MainContext::default().block_on(picture("data:image/gif;base64,R0lGODlh", "https://a.b/")).unwrap();
        wait("got.data");
        assert_eq!(read("got.data"), b"GIF89a");
        assert_eq!(String::from_utf8(read("got.args")).unwrap(), "--stdin\npicture.gif\n");

        link("https://example.com/page").unwrap();
        wait("got.args");
        assert_eq!(String::from_utf8(read("got.args")).unwrap(), "--url\nhttps://example.com/page\n");

        assert!(glib::MainContext::default().block_on(picture("blob:https://a.b/123", "https://a.b/")).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
