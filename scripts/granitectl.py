#!/usr/bin/env python3
"""Former name of scripts/localfleet.py, kept so existing app configs and scripts keep working."""
import os
from pathlib import Path
import sys

target = Path(__file__).resolve().with_name('localfleet.py')
os.execv(sys.executable, [sys.executable, str(target), *sys.argv[1:]])
