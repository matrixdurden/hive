#!/usr/bin/env python3
"""engram'ın araç ikonu motorunkiyle aynı: motorun ikonunu çizer (motor/araclar/ikon.py) ve
winloom'ın kullandığı boyutları buraya kopyalar.
Çalıştır: python3 araclar/engram/ikon/ikon.py  →  araclar/engram/ikon/engram-{24..128}.png
"""

import shutil
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
MOTOR = HERE.parent / "motor"

subprocess.run([sys.executable, str(MOTOR / "araclar" / "ikon.py")], check=True)
for size in [24, 32, 40, 48, 64, 128]:
    shutil.copy(MOTOR / "assets" / f"icon-{size}.png", HERE / f"engram-{size}.png")
