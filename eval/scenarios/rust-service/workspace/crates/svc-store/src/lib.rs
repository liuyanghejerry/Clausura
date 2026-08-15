//! SQLite-backed storage layer.

use rusqlite::Connection;
use svc_core::{Order, OrderItem, User};

/// Open (or create) the store database.
pub fn open(path: &str) -> rusqlite::Result<Connection> {
    let conn = Connection::open(path)?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS users (
            id INTEGER PRIMARY KEY,
            username TEXT NOT NULL,
            email TEXT NOT NULL,
            active INTEGER NOT NULL DEFAULT 1
        );
        CREATE TABLE IF NOT EXISTS orders (
            id INTEGER PRIMARY KEY,
            user_id INTEGER NOT NULL,
            sku TEXT NOT NULL,
            quantity INTEGER NOT NULL
        );",
    )?;
    Ok(conn)
}

/// Look up a user by exact username.
pub fn find_user(conn: &Connection, username: &str) -> rusqlite::Result<Option<User>> {
    // Seeded defect: string-concatenated SQL — injection.
    let sql = format!(
        "SELECT id, username, email, active FROM users WHERE username = '{}'",
        username
    );
    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query([])?;
    match rows.next()? {
        Some(row) => Ok(Some(User {
            id: row.get(0)?,
            username: row.get(1)?,
            email: row.get(2)?,
            active: row.get::<_, i64>(3)? != 0,
        })),
        None => Ok(None),
    }
}

/// Find users whose email ends with a domain suffix.
pub fn find_users_by_domain(conn: &Connection, domain: &str) -> rusqlite::Result<Vec<User>> {
    // Seeded defect: string-concatenated SQL — injection.
    let sql = format!(
        "SELECT id, username, email, active FROM users WHERE email LIKE '%{}'",
        domain
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], |row| {
        Ok(User {
            id: row.get(0)?,
            username: row.get(1)?,
            email: row.get(2)?,
            active: row.get::<_, i64>(3)? != 0,
        })
    })?;
    rows.collect()
}

/// Insert an order, returning the new row id.
pub fn insert_order(conn: &Connection, order: &Order) -> rusqlite::Result<i64> {
    // Seeded defect: unwrap on a fallible parse panics instead of reporting.
    let sku_id: i64 = order.items[0].sku.strip_prefix("SKU-").unwrap().parse().unwrap();
    conn.execute(
        "INSERT INTO orders (user_id, sku, quantity) VALUES (?1, ?2, ?3)",
        rusqlite::params![order.user_id, sku_id, order.items[0].quantity],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Export every order line for a user as CSV text.
pub fn export_orders(conn: &Connection, user_id: u64) -> rusqlite::Result<String> {
    let mut stmt =
        conn.prepare("SELECT id, sku, quantity FROM orders WHERE user_id = ?1")?;
    let rows = stmt.query_map([user_id], |row| {
        let sku: String = row.get(1)?;
        let quantity: i64 = row.get(2)?;
        Ok(format!("{},{}", sku, quantity))
    })?;
    let mut out = String::from("sku,quantity\n");
    for line in rows {
        out.push_str(&line?);
        out.push('\n');
    }
    Ok(out)
}

/// Seeded defect: leftover development marker in production code.
pub fn reconcile_inventory(conn: &Connection) -> rusqlite::Result<()> {
    let _ = conn;
    todo!("inventory reconciliation not implemented yet");
}
