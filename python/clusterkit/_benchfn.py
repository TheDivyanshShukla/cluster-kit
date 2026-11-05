from __future__ import annotations
from typing import Any


def pyver(x: Any) -> str:
    """The Python this call ran on (ck.pc_python asks the PCs with it)."""
    import sys
    return "{}.{}.{}".format(*sys.version_info[:3]) + ("" if getattr(sys, "_is_gil_enabled", lambda: True)() else "t")


def noop(x: Any) -> Any:
    """Empty work: what is left when the function costs nothing is the cluster's overhead."""
    return x
