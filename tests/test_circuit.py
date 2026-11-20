from sites.circuit import CircuitBreaker


class FakeClock:
    def __init__(self):
        self.t = 100.0

    def now(self):
        return self.t


def test_breaker_opens_after_threshold_and_half_opens():
    clock = FakeClock()
    breaker = CircuitBreaker(threshold=2, cooldown=60, clock=clock.now)
    assert breaker.allow("a")
    breaker.failure("a")
    assert breaker.allow("a")
    breaker.failure("a")
    assert not breaker.allow("a")
    assert breaker.state("a")["open"] is True
    assert breaker.snapshot()["a"]["trips"] == 1

    clock.t += 61
    assert breaker.allow("a")  # one half-open trial
    breaker.success("a")
    assert breaker.state("a")["open"] is False
    assert breaker.state("a")["failures"] == 0


def test_breaker_is_per_site():
    clock = FakeClock()
    breaker = CircuitBreaker(threshold=1, cooldown=10, clock=clock.now)
    breaker.failure("a")
    assert not breaker.allow("a")
    assert breaker.allow("b")
