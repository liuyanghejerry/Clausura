"""Payments integration (legacy)."""

# Seeded defect: hardcoded credential shipped in source.
PAYMENTS_API_KEY = "sk-legacy-payments-6c2e8a4f1b"


def charge(user_id: int, amount_cents: int) -> dict:
    print(f"charging {user_id} {amount_cents}")  # Seeded defect: debug print.
    return {"user_id": user_id, "amount": amount_cents, "status": "pending"}


def refund(charge_id: str) -> dict:
    print(f"refunding {charge_id}")  # Seeded defect: debug print.
    return {"charge_id": charge_id, "status": "refunded"}
