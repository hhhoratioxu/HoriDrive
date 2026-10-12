"""Package actual completed builds only; never creates a placeholder executable."""
import hashlib
import json
import os
from pathlib import Path
import shutil
import sys
import zipfile

targets = {
    "x86_64-pc-windows-msvc": "windows-x64",
    "aarch64-apple-darwin": "macos-arm64",
    "x86_64-apple-darwin": "macos-x64",
}
target, output = sys.argv[1:]
platform = targets[target]
root = Path(__file__).resolve().parent.parent
version = json.loads((root / "desktop/src-tauri/tauri.conf.json").read_text())["version"]
build = root / "target" / target / "release"
output = Path(output)
output.mkdir(parents=True, exist_ok=True)
windows = platform.startswith("windows")
installers = list((build / "bundle" / ("nsis" if windows else "dmg")).glob("*.exe" if windows else "*.dmg"))
if len(installers) != 1:
    raise RuntimeError(f"Expected one completed installer, found {len(installers)}")
cli = build / ("horidrive.exe" if windows else "horidrive")
for file in [installers[0], cli]:
    if not file.is_file() or file.stat().st_size == 0:
        raise RuntimeError(f"Build artifact missing: {file}")
installer = output / f"HoriDrive_{version}_{platform}{'_setup.exe' if windows else '.dmg'}"
shutil.copyfile(installers[0], installer)
archive = output / f"HoriDrive-cli_{version}_{platform}.zip"
with zipfile.ZipFile(archive, "w", compression=zipfile.ZIP_DEFLATED) as package:
    package.write(cli, cli.name)
    package.write(root / "LICENSE", "LICENSE")
    package.writestr("README.txt", "HoriDrive CLI — Your Cloud. Your Rules.\n\n"
        "Run horidrive --help. Choose a data folder with --root.\n"
        "Public certificates must be exchanged and their full fingerprints independently verified.\n"
        "See https://github.com/hhhoratioxu/HoriDrive#readme for pairing and sync instructions.\n")
hashes = {}
for file in [installer, archive]:
    with file.open("rb") as stream:
        hashes[file.name] = hashlib.file_digest(stream, "sha256").hexdigest()
manifest = {"commit": os.environ.get("GITHUB_SHA", "local"), "target": target,
            "profile": "release", "signed": False, "sha256": hashes}
(output / f"BUILD-{platform}.json").write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
(output / f"SHA256SUMS-{platform}.txt").write_text(
    "".join(f"{digest}  {name}\n" for name, digest in hashes.items()), encoding="utf-8")
print(f"Packaged {platform}: " + ", ".join(hashes))
