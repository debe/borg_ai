import unittest
from decimal import Decimal

from ledger import Ledger, Money, Transaction
from ledger.money import round_cents
from ledger.plugins import RATES


class Invariants(unittest.TestCase):
    def test_bankers_rounding_is_kept(self):
        self.assertEqual(round_cents(Decimal("2.5")), 2)
        self.assertEqual(round_cents(Decimal("3.5")), 4)
        self.assertEqual(Money.of("0.125"), Money(12))

    def test_existing_behaviour(self):
        l = Ledger()
        l.add(Transaction.on("2026-01-02", Money.of("10.50"), "food"))
        l.add(Transaction.on("2026-01-01", Money.of("-2.25"), "fees"))
        self.assertEqual(l.balance(), Money(825))
        self.assertIn(("EUR", "USD"), RATES)
