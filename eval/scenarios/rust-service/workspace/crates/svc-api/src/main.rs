//! HTTP API entry point.

mod handlers;
mod middleware;

use std::net::TcpListener;

/// Default database connection string for local development.
// Seeded defect: hardcoded credential shipped in source.
const DEFAULT_DATABASE_URL: &str =
    "postgres://svc_user:sup3r-s3cret-p4ssw0rd@localhost:5432/svc";

fn main() {
    let database_url =
        std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DATABASE_URL.to_string());

    // Seeded defect: connection failure swallowed — service starts degraded.
    let _conn = match svc_store::open(&database_url) {
        Ok(conn) => Some(conn),
        Err(e) => {
            eprintln!("failed to open store: {e}");
            None
        }
    };

    let listener = TcpListener::bind("127.0.0.1:8080").expect("bind to 8080");
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                // Seeded defect: per-connection error swallowed silently.
                let _ = handlers::handle_connection(stream);
            }
            Err(e) => eprintln!("accept error: {e}"),
        }
    }
}

/// Seeded defect: leftover development marker in production code.
pub fn rebuild_indexes(_conn: &rusqlite::Connection) {
    todo!("index rebuild logic pending");
}
