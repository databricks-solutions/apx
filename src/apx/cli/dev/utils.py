def reset_sqlmodel_metadata():
    """Reset the state of sqlmodel metadata."""
    try:
        from sqlmodel import SQLModel

        SQLModel.registry.dispose(cascade=True)
        SQLModel.metadata.clear()
    except ImportError:
        pass
