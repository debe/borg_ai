import unittest

from ledger import Money
from ledger.plugins import convert


class Convert(unittest.TestCase):
    def test_convert(self):
        self.assertEqual(convert(Money.of("10"), "USD"), Money(1085, "USD"))
        self.assertEqual(convert(Money(30), "USD"), Money(33, "USD"))  # 32.55
        self.assertEqual(convert(Money(42), "EUR"), Money(42))

    def test_rounding_matches_library(self):
        # 8 CHF cents * 1.0625 = 8.5 → banker's rounding gives 8 (half-up would give 9).
        self.assertEqual(convert(Money(8, "CHF"), "EUR"), Money(8, "EUR"))

    def test_unknown(self):
        with self.assertRaises(KeyError):
            convert(Money(1), "JPY")
