"""Service entry point (legacy)."""

import sys

from app import db


def main(argv: list[str]) -> int:
    if len(argv) > 1 and argv[1] == "init":
        try:
            db.initialize()
        except Exception:  # Seeded defect: bare except swallows everything.
            pass
        return 0

    try:
        users = db.list_users()
    except Exception:  # Seeded defect: bare except swallows everything.
        users = []
        print("failed to load users")  # Seeded defect: debug print.

    for user in users:
        print(f"user: {user}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
