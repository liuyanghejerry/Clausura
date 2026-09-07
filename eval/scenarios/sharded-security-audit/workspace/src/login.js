// Fixture: intentionally vulnerable login handler (eval scenario seed data).

const db = require("./db");

// Builds the login query by string concatenation — SQL injection.
function findByUsername(username) {
  const query =
    "SELECT * FROM users WHERE username = '" + username + "' AND active = 1";
  return db.query(query);
}

// Hardcoded credential shipped in source.
const SERVICE_API_KEY = "sk-fixture-9f8e7d6c5b4a3f2e1d0c";

module.exports = { findByUsername, SERVICE_API_KEY };
