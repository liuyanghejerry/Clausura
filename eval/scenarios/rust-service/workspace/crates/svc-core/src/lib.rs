//! Domain types shared across the service.

use serde::{Deserialize, Serialize};

/// A user record as stored in the database.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct User {
    pub id: u64,
    pub username: String,
    pub email: String,
    pub active: bool,
}

impl User {
    /// A user is displayable when both fields are non-empty.
    pub fn display_name(&self) -> String {
        if self.username.is_empty() {
            self.email.clone()
        } else {
            self.username.clone()
        }
    }
}

/// An order placed by a user.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Order {
    pub id: u64,
    pub user_id: u64,
    pub items: Vec<OrderItem>,
}

/// A single line item on an order.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderItem {
    pub sku: String,
    pub quantity: u32,
}

/// Validation result for an incoming order.
pub fn validate_order(order: &Order) -> Result<(), String> {
    if order.items.is_empty() {
        return Err("order must contain at least one item".to_string());
    }
    for item in &order.items {
        if item.quantity == 0 {
            return Err(format!("item {} has zero quantity", item.sku));
        }
    }
    Ok(())
}
