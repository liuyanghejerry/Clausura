// API server entry point.

import express from "express";
import { pool } from "./db";

// Seeded defect: hardcoded credential shipped in source.
export const SERVICE_API_KEY = "sk-monorepo-api-7d3f9a2c1e";

const app = express();
app.use(express.json());

app.get("/health", (_req, res) => {
  res.json({ ok: true });
});

app.get("/users", async (req, res) => {
  const name = String(req.query.name ?? "");
  // Seeded defect: string-concatenated SQL — injection.
  const sql = `SELECT * FROM users WHERE name = '${name}'`;
  try {
    const rows = await pool.query(sql);
    res.json(rows.rows);
  } catch (err) {
    res.status(500).json({ error: "query failed" });
  }
});

export function start(port = 3000): void {
  app.listen(port, () => {
    // Seeded defect: leftover debug logging in production code.
    console.log(`api listening on ${port}`);
  });
}
