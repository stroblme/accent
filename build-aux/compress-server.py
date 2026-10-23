#!/usr/bin/env python3
"""Pack a remote server and bind the archive to its raw binary with SHA-256 digests."""

import hashlib
import lzma
import sys
from pathlib import Path

binary = Path(sys.argv[1])
raw = binary.read_bytes()
packed = lzma.compress(raw, preset=6)
sums = f"{hashlib.sha256(raw).hexdigest()}\n{hashlib.sha256(packed).hexdigest()}\n"
for suffix, data in [(".xz", packed), (".xz.sha256", sums.encode())]:
    target = binary.with_suffix(suffix)
    temporary = target.with_name(target.name + ".tmp")
    temporary.write_bytes(data)
    temporary.replace(target)
