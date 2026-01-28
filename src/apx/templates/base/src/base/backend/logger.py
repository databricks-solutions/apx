"""Application logger.

Logging configuration is handled by APX via uvicorn's log config.
This module simply exposes a logger instance for the application.
"""

import logging
from typing import Optional

from .._metadata import app_name

# Default logger for the application
logger = logging.getLogger(app_name)


def get_logger(name: Optional[str] = None) -> logging.Logger:
    """
    Get a logger instance.

    Args:
        name: Logger name. If None, returns the default app logger.

    Returns:
        Logger instance.
    """
    if name is None:
        return logger
    return logging.getLogger(name)
