"""Dependency-free playground result formatting."""


def format_elapsed(seconds: float) -> str:
    """Format sub-second durations as milliseconds and longer ones as seconds."""
    return f"{seconds * 1000:.0f} ms" if seconds < 1 else f"{seconds:.2f} s"
