from dataclasses import dataclass

from ledger.clock import parse_date
from ledger.money import Money


@dataclass(frozen=True)
class Transaction:
    when: object
    amount: Money
    category: str
    memo: str = ""

    @classmethod
    def on(cls, day, amount, category, memo=""):
        return cls(parse_date(day), amount, category, memo)


class Ledger:
    def __init__(self, currency="EUR"):
        self.currency = currency
        self._txs = []

    def add(self, tx):
        if tx.amount.currency != self.currency:
            raise ValueError("currency mismatch")
        self._txs.append(tx)
        self._txs.sort(key=lambda t: t.when)

    def transactions(self):
        return list(self._txs)

    def balance(self):
        total = Money(0, self.currency)
        for tx in self._txs:
            total = total + tx.amount
        return total
