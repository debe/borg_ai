import os

_PREFIX = "LEDGER_"


def setting(name, default=None):
    return os.environ.get(_PREFIX + name.upper(), default)
