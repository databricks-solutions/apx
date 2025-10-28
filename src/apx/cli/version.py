import inspect
from inspect import Parameter
from functools import wraps
from typing import Callable, TypeVar, Any
import typer

T = TypeVar("T", bound=Callable[..., Any])  # pyright: ignore[reportExplicitAny]


def version_callback(value: bool) -> None:
    """Callback for version option."""
    from apx._version import version as apx_version
    from rich import print

    if value:
        print(f"apx version: {apx_version}")
        raise typer.Exit(code=0)


def with_version(func: T) -> T:
    """Decorator that adds a --version option (eager) to a Typer/Click command."""
    sig = inspect.signature(func)

    version_param = Parameter(
        "version",
        kind=Parameter.KEYWORD_ONLY,  # makes it an option, not an argument
        default=typer.Option(
            None,
            "--version",
            callback=version_callback,
            is_eager=True,
            help="Show the version and exit.",
        ),
        annotation=bool | None,
    )

    # Append only if not already present
    if "version" not in sig.parameters:
        new_params = list(sig.parameters.values()) + [version_param]
        new_sig = sig.replace(parameters=new_params)
    else:
        new_sig = sig

    @wraps(func)
    def wrapper(  # pyright: ignore[reportAny]
        *args: Any,  # pyright: ignore[reportExplicitAny, reportAny]
        **kwargs: dict[str, Any],  # pyright: ignore[reportExplicitAny]
    ) -> Any:  # pyright: ignore[reportExplicitAny]
        # Typer will pass 'version' in kwargs; we don't forward it to your function
        kwargs.pop("version", None)
        return func(*args, **kwargs)  # pyright: ignore[reportAny]

    # Make Typer "see" the modified signature
    wrapper.__signature__ = new_sig  # pyright: ignore[reportAttributeAccessIssue]
    return wrapper  # pyright: ignore[reportReturnType]
