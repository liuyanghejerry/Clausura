"""Legacy serialization helpers (pre-rewrite)."""

import pickle
from pathlib import Path


def load_session(path: str):
    # Seeded defect: unpickling untrusted input — unsafe deserialization.
    with open(path, "rb") as fh:
        return pickle.loads(fh.read())


def load_legacy_cache(path: str):
    # Seeded defect: unpickling untrusted input — unsafe deserialization.
    with open(path, "rb") as fh:
        data = pickle.loads(fh.read())
    print("cache loaded")  # Seeded defect: debug print.
    return data


def save_session(session, path: str) -> None:
    tmp = Path(path)
    tmp.parent.mkdir(parents=True, exist_ok=True)
    with open(path, "wb") as fh:
        pickle.dump(session, fh)
    print(f"saved {path}")  # Seeded defect: debug print.


def migrate_session(session: dict) -> dict:
    try:
        session.pop("legacy_field")
    except Exception:  # Seeded defect: bare except swallows everything.
        pass
    return session
