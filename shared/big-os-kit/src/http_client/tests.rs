// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

use super::*;

#[test]
fn stream_cap_is_enforced() {
    let mut reader = &b"abcdef"[..];
    assert!(matches!(
        read_stream_capped(&mut reader, 3),
        Err(HttpError::BodyTooLarge(3))
    ));
}

#[test]
fn read_cap_accepts_u64_max() {
    let mut reader = &b"abcdef"[..];
    assert_eq!(read_capped(&mut reader, u64::MAX).unwrap(), b"abcdef");
}

fn arguments(command: &Command) -> Vec<String> {
    command
        .get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect()
}

#[test]
fn protocol_policy_restricts_redirects() {
    let mut command = Command::new(CURL);
    restrict_protocols(&mut command, "http://example.com/");
    assert_eq!(
        arguments(&command),
        ["--proto", "=http,https", "--proto-redir", "=http,https"]
    );
    let mut command = Command::new(CURL);
    restrict_protocols(&mut command, "HTTPS://example.com/");
    assert_eq!(
        arguments(&command),
        ["--proto", "=http,https", "--proto-redir", "=https"]
    );
}

/// A stream has no total deadline, so a long download is not cut short.
#[test]
fn stream_deadline_is_a_stall_limit() {
    let total = arguments(&base_curl(Deadline::Total(Duration::from_secs(15)), ""));
    assert!(total.windows(2).any(|pair| pair == ["--max-time", "15"]));
    let stall = arguments(&base_curl(Deadline::Stall(Duration::from_secs(30)), ""));
    assert!(!stall.iter().any(|arg| arg == "--max-time"));
    assert!(
        stall
            .windows(4)
            .any(|args| args == ["--speed-limit", "1", "--speed-time", "30"])
    );
}

/// Serve one canned answer per connection, in order, on a local port.
fn serve(answers: Vec<&'static [u8]>) -> (String, std::thread::JoinHandle<()>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        for answer in answers {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 4096];
            let _ = stream.read(&mut request).unwrap();
            stream.write_all(answer).unwrap();
        }
    });
    (url, server)
}

#[test]
fn retry_follows_a_server_error() {
    let (url, server) = serve(vec![
        b"HTTP/1.1 503 Busy\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
    ]);
    assert_eq!(http_get_with_retry(&url, &[]).as_deref(), Some("ok"));
    server.join().unwrap();
    let (url, server) = serve(vec![
        b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
    ]);
    assert_eq!(http_get_with_retry(&url, &[]), None);
    server.join().unwrap();
}

/// A redirect is followed, unless a Referer would go to the other host.
#[test]
fn capped_get_keeps_a_referer_home() {
    let target = serve(vec![
        b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\nConnection: close\r\n\r\nart",
    ]);
    let redirect = format!(
        "HTTP/1.1 302 Found\r\nLocation: {}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        target.0
    )
    .leak()
    .as_bytes();
    let (url, server) = serve(vec![redirect, redirect]);
    let followed = http_get_bytes_capped(
        &url,
        &RequestHeaders::default(),
        1024,
        Duration::from_secs(5),
    )
    .unwrap();
    assert_eq!(
        (followed.status, followed.bytes.as_slice()),
        (200, &b"art"[..])
    );
    let headers = RequestHeaders {
        referer: "http://provider.example/",
        ..RequestHeaders::default()
    };
    let kept = http_get_bytes_capped(&url, &headers, 1024, Duration::from_secs(5)).unwrap();
    assert_eq!(kept.status, 302);
    server.join().unwrap();
    target.1.join().unwrap();
}

#[test]
fn refused_connection_reports_curl_exit_code() {
    let error = http_get_bytes_capped_with_extra_headers(
        "http://127.0.0.1:1/",
        &RequestHeaders::default(),
        &[],
        1024,
        Duration::from_secs(5),
    )
    .unwrap_err();
    assert!(matches!(error, HttpError::CurlExited(7)), "{error}");
    assert_eq!(error.to_string(), "curl exited with code 7");
}

#[test]
fn credentialed_get_refuses_redirects() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0_u8; 4096];
        let _ = stream.read(&mut request).unwrap();
        stream
            .write_all(
                b"HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:1/\r\n\
                  Content-Length: 0\r\nConnection: close\r\n\r\n",
            )
            .unwrap();
    });
    let response = http_get_bytes_capped_with_extra_headers(
        &url,
        &RequestHeaders::default(),
        &[("x-api-key", "secret")],
        1024,
        Duration::from_secs(5),
    )
    .unwrap();
    server.join().unwrap();
    assert_eq!(response.status, 302);
}

/// A POST carries its caller's API key, so a redirect comes back as the
/// answer instead of taking the key to another host.
#[test]
fn credentialed_post_refuses_redirects() {
    let (url, server) = serve(vec![
        b"HTTP/1.1 307 Temporary Redirect\r\nLocation: http://127.0.0.1:1/\r\n\
          Content-Length: 0\r\nConnection: close\r\n\r\n",
    ]);
    let response = http_post_json_stream_with_extra_headers(
        &url,
        "{}",
        &RequestHeaders::default(),
        &[("x-api-key", "secret")],
        Duration::from_secs(5),
    )
    .unwrap();
    server.join().unwrap();
    assert_eq!(response.status, 307);
}

/// A stream follows redirects as the capped GET does: not with a Referer.
#[test]
fn stream_keeps_a_referer_home() {
    let (url, server) = serve(vec![
        b"HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:1/\r\n\
          Content-Length: 0\r\nConnection: close\r\n\r\n",
    ]);
    let headers = RequestHeaders {
        referer: "http://provider.example/",
        ..RequestHeaders::default()
    };
    let response = http_get_stream(&url, &headers, Duration::from_secs(5)).unwrap();
    server.join().unwrap();
    assert_eq!(response.status, 302);
}

/// A name that does not resolve is not asked again: the retry would only
/// add its delay. Timed against one attempt made after it, which a
/// resolver's cache can only make faster.
#[test]
fn unresolved_host_fails_at_once() {
    let url = "http://unresolved.invalid/";
    let started = std::time::Instant::now();
    assert_eq!(http_get_with_retry(url, &[]), None);
    let retried = started.elapsed();
    let started = std::time::Instant::now();
    let single = http_get_bytes_capped(
        url,
        &RequestHeaders::default(),
        1024,
        Duration::from_secs(5),
    );
    let one_attempt = started.elapsed();
    // A resolver that answers nothing in time (curl's 28) says nothing
    // about a retry.
    if matches!(single, Err(HttpError::CurlExited(28))) {
        return;
    }
    assert!(
        matches!(
            single,
            Err(HttpError::CurlExited(CURL_COULDNT_RESOLVE_HOST))
        ),
        "{single:?}"
    );
    assert!(
        retried < one_attempt + RETRY_DELAY,
        "{retried:?} against one attempt of {one_attempt:?}"
    );
}

/// No argv carries a NUL, and curl's config file would cut the URL at it.
#[test]
fn nul_in_url_is_refused() {
    let error = http_get_bytes_capped(
        "http://127.0.0.1:1/\0x",
        &RequestHeaders::default(),
        1024,
        Duration::from_secs(5),
    )
    .unwrap_err();
    assert!(matches!(error, HttpError::NulInUrl), "{error}");
}

/// The cap counts every byte of a block, its blank line included.
#[test]
fn header_block_stops_at_its_cap() {
    let block = |length: usize| {
        let mut block = vec![b'x'; length - 4];
        block.extend_from_slice(b"\r\n\r\n");
        block
    };
    let whole = block(MAX_HEADER_BYTES);
    assert_eq!(
        read_one_header_block(&mut whole.as_slice()).unwrap().len(),
        MAX_HEADER_BYTES
    );
    let over = block(MAX_HEADER_BYTES + 1);
    assert!(matches!(
        read_one_header_block(&mut over.as_slice()),
        Err(HttpError::HeadersTooLarge)
    ));
}

/// A body cut short ends in an error, not in a shorter body.
#[test]
fn cut_stream_reads_as_an_error() {
    let (url, server) = serve(vec![
        b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\nConnection: close\r\n\r\nshort",
    ]);
    let mut response =
        http_get_stream(&url, &RequestHeaders::default(), Duration::from_secs(5)).unwrap();
    let read = read_stream_capped(response.reader.as_mut(), 1024);
    server.join().unwrap();
    assert!(read.is_err(), "{read:?}");
}

/// The server sees the path as asked: curl expands no `[1-3]` or `{a,b}`.
#[test]
fn url_brackets_reach_the_server() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/x[1-3]{{a,b}}", listener.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0_u8; 4096];
        let read = stream.read(&mut request).unwrap();
        let line = String::from_utf8_lossy(&request[..read])
            .lines()
            .next()
            .unwrap_or_default()
            .to_owned();
        let answer = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{line}",
            line.len()
        );
        stream.write_all(answer.as_bytes()).unwrap();
    });
    let answer = http_get_with_headers(&url, &RequestHeaders::default());
    server.join().unwrap();
    assert_eq!(answer.as_deref(), Some("GET /x[1-3]{a,b} HTTP/1.1"));
}

/// curl reads `~/.curlrc` unless `--disable` comes first.
#[test]
fn curl_ignores_the_users_curlrc() {
    let command = base_curl(Deadline::Total(Duration::from_secs(1)), "");
    assert_eq!(
        arguments(&command).first().map(String::as_str),
        Some("--disable")
    );
}

/// Under `https_proxy` or `ALL_PROXY`, which curl honors, `--include`
/// prints the proxy's `200 Connection established` first, and it would
/// read as the server's answer.
#[test]
fn proxy_answer_stays_out_of_headers() {
    let command = base_curl(Deadline::Total(Duration::from_secs(1)), "");
    assert!(
        arguments(&command)
            .iter()
            .any(|argument| argument == "--suppress-connect-headers")
    );
}
