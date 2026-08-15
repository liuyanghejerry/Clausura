// PostgreSQL connection pool.

import { Pool } from "pg";

export const pool = new Pool({
  host: process.env.PGHOST ?? "localhost",
  port: 5432,
  database: process.env.PGDATABASE ?? "monorepo",
  user: process.env.PGUSER ?? "api",
  password: process.env.PGPASSWORD ?? "api",
});

export async function close(): Promise<void> {
  await pool.end();
}
