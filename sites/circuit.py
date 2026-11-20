"""Per-site circuit breaker with an injectable clock (no network in tests)."""
from __future__ import annotations

import threading
import time


class CircuitBreaker:
    """Open a site after repeated failures; half-open after a cooldown."""

    def __init__(self, threshold: int = 3, cooldown: float = 300.0,
                 clock=time.monotonic):
        self.threshold = threshold
        self.cooldown = cooldown
        self._clock = clock
        self._lock = threading.Lock()
        self._state: dict[str, dict] = {}

    def _get(self, site: str) -> dict:
        return self._state.setdefault(
            site, {"failures": 0, "opened_at": None, "trips": 0})

    def allow(self, site: str) -> bool:
        """Closed: allow. Open: allow one half-open trial after the cooldown."""
        with self._lock:
            state = self._get(site)
            if state["opened_at"] is None:
                return True
            if self._clock() - state["opened_at"] >= self.cooldown:
                return True
            return False

    def success(self, site: str) -> None:
        with self._lock:
            state = self._get(site)
            state["failures"] = 0
            state["opened_at"] = None

    def failure(self, site: str) -> None:
        with self._lock:
            state = self._get(site)
            state["failures"] += 1
            if state["failures"] >= self.threshold:
                if state["opened_at"] is None:
                    state["trips"] += 1
                state["opened_at"] = self._clock()

    def is_open(self, site: str) -> bool:
        with self._lock:
            state = self._get(site)
            return (state["opened_at"] is not None
                    and self._clock() - state["opened_at"] < self.cooldown)

    def state(self, site: str) -> dict:
        with self._lock:
            state = dict(self._get(site))
        state["open"] = self.is_open(site)
        return state

    def snapshot(self) -> dict:
        with self._lock:
            sites = list(self._state)
        return {site: self.state(site) for site in sites}
