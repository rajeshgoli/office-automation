"""Shared project-name normalization helpers."""

from __future__ import annotations

import os
from typing import Optional

_PROJECT_ALIASES = {
    "office-automation": "office-automate",
}


def normalize_project_name(project_name: Optional[str]) -> str:
    """Return a canonical project identifier for paths and repo basenames."""
    if not project_name:
        return "unknown"

    normalized = project_name.strip().rstrip("/")
    if not normalized:
        return "unknown"

    basename = os.path.basename(normalized) or normalized
    basename_lower = basename.lower()

    if basename_lower == "fractal" or basename_lower.startswith("fractal-"):
        return "fractal"

    return _PROJECT_ALIASES.get(basename_lower, basename)
