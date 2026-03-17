from __future__ import annotations

import diskcache
from fastapi import APIRouter, HTTPException

from .models import Item, ItemCreate, ItemUpdate

router = APIRouter()

_CACHE_DIR = "/tmp/bench_items_cache"
_cache = diskcache.Cache(_CACHE_DIR)

_DEFAULT_ITEMS = [
    Item(
        id=i,
        name=f"Item {i}",
        description=f"Description for item {i}",
        price=round(i * 9.99, 2),
        tags=[f"tag-{i % 3}", f"tag-{i % 5}"],
    )
    for i in range(1, 11)
]


def _populate_defaults():
    _cache.clear()
    for item in _DEFAULT_ITEMS:
        _cache[f"item:{item.id}"] = item.model_dump()
    _cache["_counter"] = 10


# Auto-populate on first boot
if "_counter" not in _cache:
    _populate_defaults()


def _next_id() -> int:
    return _cache.incr("_counter")


@router.get("/echo")
def echo() -> dict[str, bool]:
    """Minimal handler — isolates framework overhead from app logic."""
    return {"echo": True}


@router.get("/health")
def health() -> dict[str, str]:
    return {"status": "ok"}


@router.get("/items", response_model=list[Item])
def list_items() -> list[Item]:
    items = []
    for key in _cache:
        if isinstance(key, str) and key.startswith("item:"):
            items.append(Item(**_cache[key]))
    items.sort(key=lambda x: x.id)
    return items


@router.get("/items/{item_id}", response_model=Item)
def get_item(item_id: int) -> Item:
    data = _cache.get(f"item:{item_id}")
    if data is None:
        raise HTTPException(status_code=404, detail="Item not found")
    return Item(**data)


@router.post("/items", response_model=Item, status_code=201)
def create_item(body: ItemCreate) -> Item:
    item = Item(id=_next_id(), **body.model_dump())
    _cache[f"item:{item.id}"] = item.model_dump()
    return item


@router.patch("/items/{item_id}", response_model=Item)
def update_item(item_id: int, body: ItemUpdate) -> Item:
    data = _cache.get(f"item:{item_id}")
    if data is None:
        raise HTTPException(status_code=404, detail="Item not found")
    existing = Item(**data)
    updated = existing.model_copy(update=body.model_dump(exclude_unset=True))
    _cache[f"item:{item_id}"] = updated.model_dump()
    return updated


@router.delete("/items/{item_id}", status_code=204)
def delete_item(item_id: int):
    _cache.pop(f"item:{item_id}", None)
    from fastapi.responses import Response
    return Response(status_code=204)


@router.post("/items/reset")
def items_reset():
    """Clear all items and repopulate with defaults."""
    _populate_defaults()
    return {"status": "reset", "items": 10}


@router.get("/profile/dump")
def profile_dump():
    """Return profiling JSONL over HTTP (for remote extraction)."""
    from fastapi.responses import Response
    from .profiling import PROFILE_PATH
    if not PROFILE_PATH.exists():
        raise HTTPException(status_code=404, detail="No profiling data")
    return Response(content=PROFILE_PATH.read_text(), media_type="application/x-ndjson")


@router.delete("/profile/reset")
def profile_reset():
    """Clear profiling data for a fresh run."""
    from . import profiling
    profiling.reset()
    return {"status": "reset"}
