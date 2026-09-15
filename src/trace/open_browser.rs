//! The mechanism behind `cq trace --open`: a tiny local HTTP server serves
//! the trace JSON, and the OS browser is pointed at whichever hosted
//! viewer's URL-loading scheme the caller wants
//! (`profiler.firefox.com/from-url/...` or `ui.perfetto.dev/#!/?url=...`).
//!
//! No bootstrap HTML page, no `postMessage` handshake: confirmed against two
//! real precedents (`samply`'s `samply load`, `google/perfetto`'s own
//! `tools/open_trace_in_ui`) that a plain `http://127.0.0.1:<port>` URL
//! fetches fine from the `https://`-hosted viewer, despite neither viewer's
//! docs stating that outright. See
//! `docs/specs/2026-09-14-cq-trace-open-design.md`.
//!
//! `ui.perfetto.dev`'s own Content-Security-Policy `connect-src` only
//! allowlists `http://127.0.0.1:9001` for local fetches (confirmed live
//! 2026-09-14: an ephemeral port gets rejected before the request even
//! leaves the page, with no network entry and no CORS error -- just a CSP
//! violation in the console). `profiler.firefox.com` has no such
//! restriction. So the port can't be ephemeral for both formats the way
//! the original design assumed; Perfetto needs the same fixed port
//! `tools/open_trace_in_ui` uses.

use anyhow::Result;

/// Starts a local httpd, serves `json_body` to any request with the given
/// CORS origin, opens the browser at `make_browser_url(local_url)`, and
/// blocks serving until Ctrl-C. `port` of `0` means OS-assigned ephemeral;
/// pass a fixed port when the hosted viewer's CSP requires one (Perfetto).
pub fn serve_and_open(
    json_body: String,
    cors_origin: &str,
    port: u16,
    make_browser_url: impl FnOnce(&str) -> String,
) -> Result<()> {
    let server = tiny_http::Server::http(("127.0.0.1", port)).map_err(|e| {
        if port == 0 {
            anyhow::anyhow!("starting local trace server: {e}")
        } else {
            anyhow::anyhow!(
                "starting local trace server on 127.0.0.1:{port}: {e}\n\
                 This port is fixed because the hosted viewer's Content-Security-Policy \
                 only allows local connections to it; free it up (check what's listening \
                 on it) and try again."
            )
        }
    })?;
    let port = server
        .server_addr()
        .to_ip()
        .expect("Server::http always binds a real IP socket, never a Unix socket")
        .port();
    let local_url = format!("http://127.0.0.1:{port}/trace.json");
    let browser_url = make_browser_url(&local_url);

    eprintln!("Serving trace at {local_url}");
    eprintln!("Opening {browser_url}");
    eprintln!("Press Ctrl-C to stop.");
    if let Err(e) = open::that(&browser_url) {
        eprintln!(
            "Couldn't open a browser automatically ({e}); open this URL yourself:\n{browser_url}"
        );
    }

    serve_forever(&server, &json_body, cors_origin);
    Ok(())
}

/// Responds to every request on `server` with `json_body` and the
/// CORS/content-type/cache headers a browser-hosted viewer needs, until the
/// server is unblocked (Ctrl-C in production; `Server::unblock()` in
/// tests). Split out from `serve_and_open` so tests can exercise the real
/// request/response behavior without ever calling `open::that`.
fn serve_forever(server: &tiny_http::Server, json_body: &str, cors_origin: &str) {
    for request in server.incoming_requests() {
        let response = tiny_http::Response::from_string(json_body.to_string())
            .with_header(content_type_json())
            .with_header(cors_header(cors_origin))
            .with_header(private_network_header())
            .with_header(no_cache_header());
        let _ = request.respond(response);
    }
}

fn content_type_json() -> tiny_http::Header {
    tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..])
        .expect("static header is always valid")
}

fn cors_header(origin: &str) -> tiny_http::Header {
    tiny_http::Header::from_bytes(&b"Access-Control-Allow-Origin"[..], origin.as_bytes())
        .expect("cors_origin is always one of our own hardcoded https:// literals")
}

/// Chrome's Private Network Access check blocks a public HTTPS page (the
/// hosted viewer) from fetching a private-network address like
/// `127.0.0.1` unless the response opts in with this header — without it,
/// the browser's preflight for the request silently never resolves the
/// fetch. See `Access-Control-Request-Private-Network` in the CORS
/// preflight.
fn private_network_header() -> tiny_http::Header {
    tiny_http::Header::from_bytes(&b"Access-Control-Allow-Private-Network"[..], &b"true"[..])
        .expect("static header is always valid")
}

fn no_cache_header() -> tiny_http::Header {
    tiny_http::Header::from_bytes(&b"Cache-Control"[..], &b"no-cache"[..])
        .expect("static header is always valid")
}

/// Percent-encode the handful of characters that ever appear in a
/// `http://127.0.0.1:<port>/trace.json`-shaped local URL and aren't already
/// URL-safe: `:` and `/`. Not a general-purpose percent-encoder — scoped to
/// this one fixed input shape, so no crate is needed for it.
pub fn percent_encode(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            ':' => "%3A".to_string(),
            '/' => "%2F".to_string(),
            other => other.to_string(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpStream;

    #[test]
    fn percent_encode_escapes_colon_and_slash() {
        assert_eq!(
            percent_encode("http://127.0.0.1:4242/trace.json"),
            "http%3A%2F%2F127.0.0.1%3A4242%2Ftrace.json"
        );
    }

    #[test]
    fn serve_forever_responds_with_the_json_body_and_headers() {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let port = server.server_addr().to_ip().unwrap().port();
        let json = r#"{"hello":"world"}"#.to_string();

        std::thread::scope(|scope| {
            scope.spawn(|| serve_forever(&server, &json, "https://example.com"));

            let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
            stream
                .write_all(
                    b"GET /trace.json HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n",
                )
                .unwrap();
            let mut response = String::new();
            stream.read_to_string(&mut response).unwrap();

            assert!(response.starts_with("HTTP/1.1 200"), "got: {response}");
            assert!(
                response.contains("Content-Type: application/json"),
                "got: {response}"
            );
            assert!(
                response.contains("Access-Control-Allow-Origin: https://example.com"),
                "got: {response}"
            );
            assert!(
                response.contains("Access-Control-Allow-Private-Network: true"),
                "got: {response}"
            );
            assert!(
                response.contains("Cache-Control: no-cache"),
                "got: {response}"
            );
            assert!(
                response.ends_with(r#"{"hello":"world"}"#),
                "got: {response}"
            );

            server.unblock();
        });
    }
}
