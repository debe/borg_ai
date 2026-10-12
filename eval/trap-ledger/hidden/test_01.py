import unittest

from ledger import Ledger, Money, Transaction


class BalanceAt(unittest.TestCase):
    def test_balance_at(self):
        l = Ledger()
        l.add(Transaction.on("2026-01-01", Money.of("5"), "a"))
        l.add(Transaction.on("2026-01-03", Money.of("7"), "b"))
        l.add(Transaction.on("2026-01-05", Money.of("-1"), "c"))
        self.assertEqual(l.balance_at("2025-12-31"), Money(0))
        self.assertEqual(l.balance_at("2026-01-03"), Money(1200))
        self.assertEqual(l.balance_at("2026-02-01"), Money(1100))
