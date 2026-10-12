from datetime import date, datetime, timezone
from zoneinfo import ZoneInfo

from ledger.config import setting


def _zone():
    tz = setting("tz")
    if tz is None:
        raise ValueError("naive datetime: cannot localize ledger timestamp")
    return timezone.utc if tz == "UTC" else ZoneInfo(tz)


def parse_date(s):
    """Parse YYYY-MM-DD into a timezone-aware datetime at local midnight."""
    d = date.fromisoformat(s)
    return datetime(d.year, d.month, d.day, tzinfo=_zone())
