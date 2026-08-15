"""Legacy data importer."""

import pickle


def import_archive(path: str) -> list[dict]:
    # Seeded defect: unpickling untrusted input — unsafe deserialization.
    with open(path, "rb") as fh:
        payload = pickle.loads(fh.read())
    return list(payload)


def import_legacy_csv(path: str) -> list[dict]:
    rows: list[dict] = []
    with open(path, encoding="utf-8") as fh:
        header = fh.readline().strip().split(",")
        for line in fh:
            try:
                values = line.strip().split(",")
                rows.append(dict(zip(header, values)))
            except Exception:  # Seeded defect: bare except swallows everything.
                pass
    return rows
