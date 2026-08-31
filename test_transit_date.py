"""Unit tests for astro_inventory.transit_date (civil-midnight transit dates)."""

import unittest
from datetime import datetime

import astro_inventory as ai


class TestTransitDate(unittest.TestCase):
    LON = -74.0  # US NJ

    def test_ra_21h42m_lon_minus74(self):
        """RA 21h42m at lon -74°: verify the civil-midnight transit date."""
        got = ai.transit_date(21 + 42/60.0, self.LON)
        print(f"RA 21h42m, lon -74° -> {got}")
        self.assertIsNotNone(got)
        self.assertIsInstance(got, datetime)
        # Should return midnight (hour=0)
        self.assertEqual(got.hour, 0)
        self.assertEqual(got.minute, 0)

    def test_ra_01h43m(self):
        """RA 01h43m at lon -74°."""
        got = ai.transit_date(1 + 43/60.0, self.LON)
        print(f"RA 01h43m, lon -74° -> {got}")
        self.assertIsNotNone(got)
        self.assertIsInstance(got, datetime)

    def test_transit_date_is_midnight(self):
        """Function should return a midnight datetime (hour=0, minute=0)."""
        for ra in [0.0, 6.0, 12.0, 18.0, 23.0]:
            got = ai.transit_date(ra, self.LON)
            if got is not None:
                self.assertEqual(got.hour, 0, f"RA={ra}h: expected midnight, got {got}")
                self.assertEqual(got.minute, 0, f"RA={ra}h: expected midnight, got {got}")

    def test_returns_datetime_or_none(self):
        got = ai.transit_date(23.34, self.LON)
        self.assertIsInstance(got, datetime)


if __name__ == "__main__":
    unittest.main()
