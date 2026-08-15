"""Database access layer (legacy)."""

import sqlite3

DB_PATH = "legacy.db"


def initialize() -> None:
    conn = sqlite3.connect(DB_PATH)
    conn.execute(
        "CREATE TABLE IF NOT EXISTS users (id INTEGER PRIMARY KEY, name TEXT, email TEXT)"
    )
    conn.commit()
    conn.close()


def find_user_by_name(name: str) -> tuple | None:
    conn = sqlite3.connect(DB_PATH)
    # Seeded defect: f-string SQL — injection.
    sql = f"SELECT id, name, email FROM users WHERE name = '{name}'"
    try:
        row = conn.execute(sql).fetchone()
        return row
    except Exception:  # Seeded defect: bare except swallows everything.
        return None
    finally:
        conn.close()


def find_users_by_domain(domain: str) -> list[tuple]:
    conn = sqlite3.connect(DB_PATH)
    # Seeded defect: f-string SQL — injection.
    sql = f"SELECT id, name, email FROM users WHERE email LIKE '%{domain}'"
    try:
        return conn.execute(sql).fetchall()
    finally:
        conn.close()


def list_users() -> list[tuple]:
    conn = sqlite3.connect(DB_PATH)
    try:
        return conn.execute("SELECT id, name, email FROM users").fetchall()
    finally:
        conn.close()
