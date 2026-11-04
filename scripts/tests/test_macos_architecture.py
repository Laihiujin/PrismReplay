"""Regression for the packaging script's lipo argv contract (not a macOS build).

Apple lipo consumes every argument following -verify_arch as an architecture.
Execute the actual case block in bash, with a small explicit command-contract
double, including paths with spaces and negative architecture checks.
"""
import argparse
import os
from pathlib import Path
import re
import subprocess
import sys

parser = argparse.ArgumentParser()
parser.add_argument("--script", type=Path, default=Path(__file__).parents[1] / "package-macos-dmg.sh")
args = parser.parse_args()
blocks = re.findall(r'case "\$TARGET" in\n.*?\nesac', args.script.read_text(encoding="utf-8"), re.S)
blocks = [block for block in blocks if "lipo " in block]
assert len(blocks) == 1, "Expected one architecture verification case block"
contract = r'''
set -eu
lipo() {
  local binary="$1"; shift
  if [[ "$binary" == "-verify_arch" ]]; then
    echo 'lipo contract: executable path was consumed as an architecture' >&2
    return 1
  fi
  [[ "$binary" == "$INSTALLED_APP/Contents/MacOS/$EXECUTABLE" ]] || return 2
  [[ "$1" == "-verify_arch" ]] || return 2
  shift
  [[ "$#" -gt 0 ]] || return 2
  for arch in "$@"; do
    case " $AVAILABLE_ARCHS " in
      *" $arch "*) ;;
      *) echo "lipo contract: missing architecture $arch" >&2; return 1 ;;
    esac
  done
}
'''
failed = 0
targets = [("aarch64-apple-darwin", "arm64"), ("x86_64-apple-darwin", "x86_64"), ("universal-apple-darwin", "arm64 x86_64")]
for target, archs in targets:
    for available, expected in [(archs, 0), ("wrong_architecture", 1)]:
        env = dict(os.environ, TARGET=target, AVAILABLE_ARCHS=available,
                   INSTALLED_APP="/fixture path/Prism Replay.app", EXECUTABLE="shadowverse")
        proc = subprocess.run(["bash", "-c", contract + blocks[0]], env=env, capture_output=True, text=True)
        ok = proc.returncode == expected
        print(f"{'PASS' if ok else 'FAIL'} {target} available={available}: exit={proc.returncode}, expected={expected}")
        if not ok:
            failed += 1
            print(proc.stderr, end="", file=sys.stderr)
print(f"lipo argv contract: {6-failed}/6 passed (command double; no macOS image mounted)")
sys.exit(bool(failed))
