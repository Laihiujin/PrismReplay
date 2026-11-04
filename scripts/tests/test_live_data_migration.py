"""Compile and execute the real dependency-free storage migration unit tests."""
from pathlib import Path
import subprocess,tempfile
root=Path(__file__).resolve().parents[2]
with tempfile.TemporaryDirectory(prefix="live-migration-tests-") as directory:
    executable=Path(directory)/"test-live-migration"
    subprocess.run(["rustc","--edition","2021","--test",str(root/"src-tauri/src/data_identity.rs"),"-o",str(executable)],check=True)
    subprocess.run([str(executable),"--nocapture"],check=True)
