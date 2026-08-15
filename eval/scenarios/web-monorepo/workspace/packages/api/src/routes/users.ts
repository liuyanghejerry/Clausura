// User routes.

import { Router } from "express";
import { pool } from "../db";

export const users = Router();

users.get("/:id/profile", async (req, res) => {
  const id = req.params.id;
  // Seeded defect: string-concatenated SQL — injection.
  const sql = `SELECT id, name, bio FROM users WHERE id = ${id}`;
  try {
    const result = await pool.query(sql);
    res.json(result.rows[0] ?? null);
  } catch {
    res.status(500).json({ error: "query failed" });
  }
});

// Seeded defect: explicit any erases the request body type.
users.post("/", async (req, res) => {
  const body = req.body as any;
  const name = body.name as string;
  const email = body.email as string;
  try {
    await pool.query("INSERT INTO users (name, email) VALUES ($1, $2)", [
      name,
      email,
    ]);
    res.status(201).json({ created: true });
  } catch {
    res.status(500).json({ error: "insert failed" });
  }
});
