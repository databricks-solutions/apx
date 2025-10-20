import asyncio
import logging

from rich.console import Console
from rich.markup import escape

console = Console()


def print_with_prefix(prefix: str, text: str, color: str, width: int = 10):
    """Print text with a colored prefix.

    Args:
        prefix: The prefix text to display
        text: The main text to display
        color: The color for the prefix
        width: The width to pad the prefix to (default: 10)
    """
    escaped_text = escape(text)
    escaped_prefix = escape(prefix)
    # Pad the prefix to the specified width
    padded_prefix = escaped_prefix.ljust(width)
    console.print(f"[{color}]{padded_prefix}[/] | {escaped_text}")


class PrefixedLogHandler(logging.Handler):
    """A logging handler that uses print_with_prefix to output log messages."""

    def __init__(self, prefix: str, color: str, width: int = 10):
        super().__init__()
        self.prefix = prefix
        self.color = color
        self.width = width

    def emit(self, record: logging.LogRecord):
        try:
            msg = self.format(record)
            # Determine color based on log level
            color = self.color
            if record.levelno >= logging.ERROR:
                color = "red"
            elif record.levelno >= logging.WARNING:
                color = "yellow"

            print_with_prefix(self.prefix, msg, color, width=self.width)
        except Exception:
            self.handleError(record)


async def stream_output(
    proc: asyncio.subprocess.Process, prefix: str, color: str, width: int = 10
):
    """Stream output from a subprocess with a colored prefix."""

    async def read_stream(stream, is_stderr=False):
        while True:
            line = await stream.readline()
            if not line:
                break
            _color = color if not is_stderr else "red"
            text = line.decode().rstrip()
            if text:
                print_with_prefix(prefix, text, _color, width=width)

    # Read stdout and stderr concurrently
    await asyncio.gather(
        read_stream(proc.stdout, is_stderr=False),
        read_stream(proc.stderr, is_stderr=True),
    )
