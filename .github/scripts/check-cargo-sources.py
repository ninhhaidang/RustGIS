#!/usr/bin/env python3
"""Reject Cargo source/path overrides in tracked repository configuration (Python 3.11+)."""

from pathlib import Path
import os
import subprocess
import sys
import tomllib


def check(root: Path) -> list[str]:
    """Inspect tracked modern and legacy configs, including those in nested workspaces."""
    paths = os.fsdecode(subprocess.check_output(["git", "ls-files", "-z"], cwd=root)).split("\0")
    errors = []
    for name in paths:
        path = Path(name)
        if path.name.lower() == ".cargo" and (root / path).is_symlink():
            errors.append(f"{name}: symlinked Cargo config directories are not allowed")
            continue
        if path.parent.name.lower() != ".cargo" or path.name.lower() not in {"config", "config.toml"}:
            continue
        try:
            with (root / path).open("rb") as source:
                config = tomllib.load(source)
        except (OSError, ValueError):
            errors.append(f"{name}: cannot read valid TOML")
            continue
        if any(config.get(key) for key in ("source", "paths", "patch", "include")):
            errors.append(f"{name}: Cargo source/path/patch overrides and included configs are not allowed")
    return errors


if __name__ == "__main__":
    repo = Path(__file__).resolve().parents[2]
    try:
        problems = check(repo)
    except subprocess.CalledProcessError:
        sys.exit("cannot list tracked Cargo configuration")
    for problem in problems:
        print(problem, file=sys.stderr)
    sys.exit(bool(problems))
