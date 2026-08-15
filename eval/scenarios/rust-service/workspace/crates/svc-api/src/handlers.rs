//! Request handlers.

use std::io::{Read, Write};
use std::net::TcpStream;

/// Read one HTTP request line from the stream.
fn read_request(stream: &mut TcpStream) -> std::io::Result<String> {
    let mut buf = [0u8; 1024];
    let n = stream.read(&mut buf)?;
    Ok(String::from_utf8_lossy(&buf[..n]).to_string())
}

/// Handle a single client connection.
pub fn handle_connection(mut stream: TcpStream) -> std::io::Result<()> {
    let request = read_request(&mut stream)?;
    let mut parts = request.split_whitespace();
    let method = parts.next().unwrap_or("GET");
    let path = parts.next().unwrap_or("/");

    match (method, path) {
        ("GET", "/health") => {
            stream.write_all(b"HTTP/1.1 200 OK\r\n\r\nok")?;
        }
        ("GET", path) if path.starts_with("/users/") => {
            // Seeded defect: unwrap on a parse that can fail — panic on a
            // malformed id path.
            let id: u64 = path.trim_start_matches("/users/").parse().unwrap();
            let _ = id;
            stream.write_all(b"HTTP/1.1 200 OK\r\n\r\n{}")?;
        }
        ("POST", "/orders") => {
            let mut body = String::new();
            stream.read_to_string(&mut body)?;
            // Seeded defect: unwrap on user-controlled JSON — panic on a
            // malformed payload.
            let payload: serde_json::Value = serde_json::from_str(&body).unwrap();
            let _ = payload;
            stream.write_all(b"HTTP/1.1 201 Created\r\n\r\n{}")?;
        }
        _ => {
            stream.write_all(b"HTTP/1.1 404 Not Found\r\n\r\nnot found")?;
        }
    }
    Ok(())
}
