"""Build-time metadata; never accesses user authentication or conversation data."""
import hashlib
import importlib.metadata
import json
from pathlib import Path
import shutil
import sys

destination = Path(sys.argv[1])
licenses = destination / "licenses"
licenses.mkdir(exist_ok=True)
shutil.copyfile(Path(__file__).resolve().parent.parent / "LICENSE", licenses / "RShell-Bridge.txt")
python_license = Path(sys.base_prefix) / "LICENSE.txt"
if python_license.is_file():
    shutil.copyfile(python_license, licenses / "Python.txt")
shutil.copyfile(Path(__file__).with_name("README.md"), destination / "README.md")
packages = sorted(
    ({"name": d.metadata["Name"], "version": d.version} for d in importlib.metadata.distributions()),
    key=lambda p: p["name"].lower(),
)
for distribution in importlib.metadata.distributions():
    name = distribution.metadata["Name"]
    if not all(c.isalnum() or c in "-_." for c in name):
        raise ValueError("Invalid dependency name")
    for index, relative in enumerate(distribution.files or []):
        if Path(relative).name.upper().startswith(("LICENSE", "COPYING", "NOTICE")):
            source = distribution.locate_file(relative)
            if source.is_file():
                folder = licenses / name
                folder.mkdir(exist_ok=True)
                shutil.copyfile(source, folder / f"{index}-{Path(relative).name}")
files = {}
for path in sorted(destination.rglob("*")):
    if path.is_file() and path.name != "manifest.json":
        with path.open("rb") as source:
            digest = hashlib.sha256()
            for block in iter(lambda: source.read(1024 * 1024), b""):
                digest.update(block)
        files[path.relative_to(destination).as_posix()] = digest.hexdigest()
(destination / "manifest.json").write_text(json.dumps({"protocol_version": 1, "packages": packages, "sha256": files}, indent=2), encoding="utf-8")
