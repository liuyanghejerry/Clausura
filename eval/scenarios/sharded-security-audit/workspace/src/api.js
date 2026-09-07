// Fixture: routing layer (eval scenario seed data).

function registerRoutes(app) {
  app.get("/api/users", listUsers);
  app.post("/api/users", createUser);
  app.get("/api/health", healthCheck);
  return app;
}

async function listUsers(req, res) {
  const users = await db.findAll("users");
  res.json(users);
}

module.exports = { registerRoutes };
