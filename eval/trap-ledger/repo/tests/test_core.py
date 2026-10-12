import unittest

from ledger import Ledger, Money, Transaction
from ledger.plugins import RATES


class CoreTest(unittest.TestCase):
    def test_balance(self):
        l = Ledger()
        l.add(Transaction.on("2026-01-02", Money.of("10.50"), "food"))
        l.add(Transaction.on("2026-01-01", Money.of("-2.25"), "fees"))
        self.assertEqual(l.balance(), Money(825))
        self.assertEqual(l.transactions()[0].category, "fees")

    def test_rates_loaded(self):
        self.assertIn(("EUR", "USD"), RATES)
