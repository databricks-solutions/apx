"""Thin wrapper that locates and execvp's the apx binary."""

import os
import sys
import sysconfig


def find_apx_bin() -> str:
    """Find the apx binary installed by maturin."""
    exe = "apx" + (sysconfig.get_config_var("EXE") or "")

    # Standard scripts dir (pip install / venv)
    scripts_path = os.path.join(sysconfig.get_path("scripts"), exe)
    if os.path.isfile(scripts_path):
        return scripts_path

    # User scripts dir
    try:
        user_scheme = sysconfig.get_preferred_scheme("user")
        user_path = os.path.join(sysconfig.get_path("scripts", user_scheme), exe)
        if os.path.isfile(user_path):
            return user_path
    except Exception:
        pass

    # Adjacent bin/ dir (pip install --target)
    pkg_root = os.path.dirname(os.path.dirname(__file__))
    target_path = os.path.join(pkg_root, "bin", exe)
    if os.path.isfile(target_path):
        return target_path

    raise FileNotFoundError(
        f"Could not find apx binary. Searched:\n  - {scripts_path}\n  - {target_path}"
    )


def main() -> None:
    try:
        apx = find_apx_bin()
    except FileNotFoundError as e:
        print(str(e), file=sys.stderr)
        raise SystemExit(1) from None

    if sys.platform == "win32":
        import subprocess

        raise SystemExit(subprocess.call([apx, *sys.argv[1:]]))

    os.execvp(apx, [apx, *sys.argv[1:]])


if __name__ == "__main__":
    main()
