import unittest

from ledger import Money


class Split(unittest.TestCase):
    def test_split(self):
        self.assertEqual(Money.of("10").split(3), [Money(334), Money(333), Money(333)])
        self.assertEqual(Money(5, "USD").split(2), [Money(3, "USD"), Money(2, "USD")])
        self.assertEqual(Money(7).split(1), [Money(7)])

    def test_reject(self):
        with self.assertRaises(ValueError):
            Money(1).split(0)
