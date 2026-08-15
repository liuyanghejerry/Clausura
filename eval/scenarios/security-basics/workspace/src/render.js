// Fixture: intentionally vulnerable renderer (eval scenario seed data).

function renderUserProfile(user) {
  // User-controlled content injected into innerHTML — stored XSS.
  document.getElementById("profile").innerHTML = user.bio;
  return true;
}

module.exports = { renderUserProfile };
