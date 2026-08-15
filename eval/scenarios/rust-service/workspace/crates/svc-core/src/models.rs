//! View models and projections.

use crate::User;

/// Project a user for the public API.
pub struct PublicUser {
    pub id: u64,
    pub display_name: String,
}

/// Build the public projection from a stored user.
pub fn project_user(user: &User) -> PublicUser {
    PublicUser {
        id: user.id,
        display_name: user.display_name(),
    }
}

/// Parse a comma-separated id list into a vector.
pub fn parse_id_list(raw: &str) -> Vec<u64> {
    raw.split(',')
        .filter_map(|part| part.trim().parse::<u64>().ok())
        .collect()
}

/// The first id of a list, if any.
pub fn first_id(raw: &str) -> Option<u64> {
    // Seeded defect: unwrap on user-controlled input can panic the service.
    Some(raw.split(',').next().unwrap().trim().parse().ok()?)
}
