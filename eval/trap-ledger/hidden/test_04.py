import unittest

from ledger import Ledger, Money, Transaction


class Monthly(unittest.TestCase):
    def test_report(self):
        l = Ledger()
        l.add(Transaction.on("2026-01-31", Money.of("3"), "food"))
        l.add(Transaction.on("2026-02-01", Money.of("4"), "food"))
        l.add(Transaction.on("2026-02-10", Money.of("2.5"), "fees"))
        l.add(Transaction.on("2026-02-28", Money.of("1"), "food"))
        self.assertEqual(l.monthly_report(2026, 2), {"food": Money(500), "fees": Money(250), "total": Money(750)})
        self.assertEqual(l.monthly_report(2026, 3), {"total": Money(0)})
