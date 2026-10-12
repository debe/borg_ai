from dataclasses import dataclass
from decimal import ROUND_HALF_EVEN, Decimal


def round_cents(value):
    return int(Decimal(value).quantize(Decimal("1"), rounding=ROUND_HALF_EVEN))


@dataclass(frozen=True)
class Money:
    cents: int
    currency: str = "EUR"

    @classmethod
    def of(cls, amount, currency="EUR"):
        return cls(round_cents(Decimal(str(amount)) * 100), currency)

    def __add__(self, other):
        if other.currency != self.currency:
            raise ValueError("currency mismatch")
        return Money(self.cents + other.cents, self.currency)

    def __str__(self):
        sign = "-" if self.cents < 0 else ""
        return f"{sign}{abs(self.cents) // 100}.{abs(self.cents) % 100:02d} {self.currency}"
