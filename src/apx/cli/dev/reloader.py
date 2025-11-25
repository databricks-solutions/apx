"""Centralized app loading and reloading logic for dev mode.

This module ensures the app is loaded only once per reload cycle, preventing
issues with SQLModel and other libraries that don't handle multiple imports well.
"""

import importlib
import sys
from typing import Any

from fastapi import FastAPI
from typer import Exit

from apx.utils import console


def _clear_sqlmodel_registry() -> None:
    """Clear SQLModel/SQLAlchemy registry to prevent duplicate class warnings.

    When reloading modules, SQLModel's internal registry still contains references
    to the old classes. This function clears those registries before reload.
    """
    try:
        from sqlmodel import SQLModel

        # Clear the metadata (table definitions)
        SQLModel.metadata.clear()

        # Clear the class registry to prevent "duplicate class" warnings
        # SQLModel uses _sa_registry which contains the SQLAlchemy registry
        if hasattr(SQLModel, "_sa_registry") and hasattr(
            SQLModel._sa_registry, "_class_registry"
        ):
            SQLModel._sa_registry._class_registry.clear()
    except ImportError:
        # SQLModel not installed, nothing to clear
        pass
    except Exception:
        # If clearing fails, continue anyway (better than blocking reload)
        pass


class AppReloader:
    """Manages app loading and reloading with caching to prevent duplicate imports."""

    def __init__(self) -> None:
        self._app_instance: FastAPI | None = None
        self._app_module_name: str | None = None
        self._reload_count: int = 0

    def load_app(
        self, app_module_name: str, reload: bool = False
    ) -> tuple[FastAPI, int]:
        """Load or reload the FastAPI app instance.

        Args:
            app_module_name: Module name in format "module.path:app"
            reload: If True, forces a reload even if app is cached

        Returns:
            Tuple of (app_instance, reload_count) where reload_count is incremented
            on each reload to help callers detect when a reload occurred

        Raises:
            Exit: If app loading fails
        """
        # If we have a cached app and not forcing reload, return it
        if (
            not reload
            and self._app_instance is not None
            and self._app_module_name == app_module_name
        ):
            return self._app_instance, self._reload_count

        # Parse module name
        if ":" not in app_module_name:
            console.print(
                "[red]❌ Invalid app module format. Expected format: some.package.file:app[/red]"
            )
            raise Exit(code=1)

        module_path, attribute_name = app_module_name.split(":", 1)

        # Clear module cache if reloading
        if reload or self._app_instance is not None:
            # Clear SQLModel/SQLAlchemy registries BEFORE clearing modules
            _clear_sqlmodel_registry()

            base_path = module_path.split(".")[0]
            modules_to_delete = [
                name
                for name in list(sys.modules.keys())
                if name.startswith(base_path + ".") or name == base_path
            ]
            for mod_name in modules_to_delete:
                del sys.modules[mod_name]
            self._reload_count += 1

        # Import the module
        try:
            module = importlib.import_module(module_path)
        except ImportError as e:
            console.print(f"[red]❌ Failed to import module {module_path}: {e}[/red]")
            raise Exit(code=1)

        # Get the app attribute from the module
        try:
            app_instance: Any = getattr(module, attribute_name)
        except AttributeError:
            console.print(
                f"[red]❌ Module {module_path} does not have attribute '{attribute_name}'[/red]"
            )
            raise Exit(code=1)

        if not isinstance(app_instance, FastAPI):
            console.print(
                f"[red]❌ '{attribute_name}' is not a FastAPI app instance.[/red]"
            )
            raise Exit(code=1)

        # Cache the app
        self._app_instance = app_instance
        self._app_module_name = app_module_name

        return self._app_instance, self._reload_count

    def get_app(self) -> FastAPI | None:
        """Get the currently loaded app instance without reloading.

        Returns:
            The cached app instance or None if not loaded yet
        """
        return self._app_instance

    def get_reload_count(self) -> int:
        """Get the number of times the app has been reloaded.

        Returns:
            Reload count
        """
        return self._reload_count

    def clear(self):
        """Clear the cached app instance."""
        self._app_instance = None
        self._app_module_name = None


# Global reloader instance for dev mode
_dev_reloader = AppReloader()


def load_app(app_module_name: str, reload: bool = False) -> tuple[FastAPI, int]:
    """Load or reload the FastAPI app instance using the global reloader.

    Args:
        app_module_name: Module name in format "module.path:app"
        reload: If True, forces a reload even if app is cached

    Returns:
        Tuple of (app_instance, reload_count) where reload_count is incremented
        on each reload

    Raises:
        Exit: If app loading fails
    """
    return _dev_reloader.load_app(app_module_name, reload=reload)


def get_app() -> FastAPI | None:
    """Get the currently loaded app instance without reloading.

    Returns:
        The cached app instance or None if not loaded yet
    """
    return _dev_reloader.get_app()


def get_reload_count() -> int:
    """Get the number of times the app has been reloaded.

    Returns:
        Reload count
    """
    return _dev_reloader.get_reload_count()


def clear_app():
    """Clear the cached app instance."""
    _dev_reloader.clear()
