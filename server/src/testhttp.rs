use std::io::{Read, Write};
use std::sync::mpsc::Receiver;

/// Answers each request in turn with the next `(status, body)` and hands the
/// raw requests back, so the wire a caller puts out can be asserted without
/// the real endpoint.
pub fn serve(responses: Vec<(&'static str, &'static str)>) -> (String, Receiver<String>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for (status, body) in responses {
            let Ok((mut sock, _)) = listener.accept() else { return };
            let Some(raw) = read_request(&mut sock) else { return };
            let _ = sock.write_all(
                format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            );
            let _ = tx.send(raw);
        }
    });
    (base, rx)
}

/// Answers one request with a `text/event-stream` body: the head after
/// `head_delay`, then each chunk after its own delay.
pub fn serve_stream(
    head_delay: std::time::Duration,
    chunks: Vec<(std::time::Duration, &'static str)>,
) -> (String, Receiver<String>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let Ok((mut sock, _)) = listener.accept() else { return };
        let Some(raw) = read_request(&mut sock) else { return };
        let _ = tx.send(raw);
        std::thread::sleep(head_delay);
        let len: usize = chunks.iter().map(|(_, c)| c.len()).sum();
        let head = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {len}\r\nConnection: close\r\n\r\n"
        );
        if sock.write_all(head.as_bytes()).is_err() {
            return;
        }
        for (delay, chunk) in chunks {
            std::thread::sleep(delay);
            if sock.write_all(chunk.as_bytes()).is_err() {
                return;
            }
        }
    });
    (base, rx)
}

fn read_request(sock: &mut std::net::TcpStream) -> Option<String> {
    let mut raw = Vec::new();
    let mut buf = [0u8; 1024];
    loop {
        let n = sock.read(&mut buf).ok()?;
        raw.extend_from_slice(&buf[..n]);
        let text = String::from_utf8_lossy(&raw).to_string();
        let Some(head_end) = text.find("\r\n\r\n") else {
            if n == 0 {
                break;
            }
            continue;
        };
        let want: usize = text[..head_end]
            .lines()
            .find_map(|l| l.strip_prefix("content-length: ").or(l.strip_prefix("Content-Length: ")))
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(0);
        if raw.len() >= head_end + 4 + want || n == 0 {
            break;
        }
    }
    Some(String::from_utf8_lossy(&raw).to_string())
}

pub fn body_json(raw: &str) -> serde_json::Value {
    let (_, body) = raw.split_once("\r\n\r\n").expect("a request with a body");
    serde_json::from_str(body).unwrap_or_else(|e| panic!("body {body:?}: {e}"))
}
