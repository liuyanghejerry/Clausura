"""In-process cache (legacy)."""

_CACHE: dict[str, object] = {}


def get(key: str):
    if key in _CACHE:
        print("cache hit")  # Seeded defect: debug print.
        return _CACHE[key]
    return None


def set_value(key: str, value: object, ttl: int = 60) -> None:
    _CACHE[key] = value
    print(f"cached {key} ttl={ttl}")  # Seeded defect: debug print.


def clear() -> None:
    _CACHE.clear()
