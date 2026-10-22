"""Rebuild PhotoCraft from this checkout and copy it to the install folder.

Usage:
    python install.py                 # installs to C:\\Apps\\Photocraft
    python install.py --dest D:\\Apps\\Photocraft
    python install.py --no-pause      # don't wait for Enter at the end

Double-clicking the file also works: the window stays open at the end so you can read the result.
"""

import argparse
import os
import shutil
import subprocess
import sys
import time
from pathlib import Path

DEFAULT_DEST = Path(r"C:\Apps\Photocraft")
EXE = "photocraft.exe" if os.name == "nt" else "photocraft"


def main() -> int:
    parser = argparse.ArgumentParser(description="Rebuild PhotoCraft and copy it to the install folder.")
    parser.add_argument("--dest", type=Path, default=DEFAULT_DEST, help=f"install folder (default {DEFAULT_DEST})")
    parser.add_argument("--no-pause", action="store_true", help="don't wait for Enter before closing")
    args = parser.parse_args()

    repo = Path(__file__).resolve().parent
    print(f"Building PhotoCraft (release) in {repo} ...", flush=True)
    try:
        build = subprocess.run(["cargo", "build", "--release", "-p", "photocraft"], cwd=repo)
    except FileNotFoundError:
        print("cargo was not found. Install Rust from https://rustup.rs and try again.")
        return 1
    if build.returncode != 0:
        print(f"Build failed; the installed copy in {args.dest} was left untouched.")
        return build.returncode

    built = repo / "target" / "release" / EXE
    if not built.is_file():
        print(f"The build finished but {built} is missing.")
        return 1

    args.dest.mkdir(parents=True, exist_ok=True)
    target = args.dest / EXE
    interactive = sys.stdin is not None and sys.stdin.isatty()
    while True:
        try:
            shutil.copy2(built, target)
            break
        except PermissionError:
            # Windows locks a running .exe, so a failed copy almost always means it's open.
            print()
            print("=" * 72)
            print(f"  NOT INSTALLED: {target} is in use.")
            print("  Close PhotoCraft (every window), then press Enter to try again.")
            print("=" * 72)
            if not interactive:
                return 1
            if input("Press Enter to retry, or type q to quit: ").strip().lower() == "q":
                print(f"Not installed; {target} is still the old version.")
                return 1
    print(f"Installed to {args.dest} (built {time.ctime(built.stat().st_mtime)}).")
    return 0


if __name__ == "__main__":
    code = main()
    if "--no-pause" not in sys.argv and sys.stdin is not None and sys.stdin.isatty():
        input("Press Enter to close...")
    sys.exit(code)
