from __future__ import annotations

from fastapi import APIRouter, HTTPException

from .models import Item, ItemCreate, ItemUpdate

router = APIRouter()

_COUNTER: int = 10
ITEMS: dict[int, Item] = {
    i: Item(
        id=i,
        name=f"Item {i}",
        description=f"Description for item {i}",
        price=round(i * 9.99, 2),
        tags=[f"tag-{i % 3}", f"tag-{i % 5}"],
    )
    for i in range(1, 11)
}


def _next_id() -> int:
    global _COUNTER  # noqa: PLW0603
    _COUNTER += 1
    return _COUNTER


@router.get("/echo")
def echo() -> dict[str, bool]:
    """Minimal handler — isolates framework overhead from app logic."""
    return {"echo": True}


@router.get("/health")
def health() -> dict[str, str]:
    return {"status": "ok"}


@router.get("/items", response_model=list[Item])
def list_items() -> list[Item]:
    return list(ITEMS.values())


@router.get("/items/{item_id}", response_model=Item)
def get_item(item_id: int) -> Item:
    if item_id not in ITEMS:
        raise HTTPException(status_code=404, detail="Item not found")
    return ITEMS[item_id]


@router.post("/items", response_model=Item, status_code=201)
def create_item(body: ItemCreate) -> Item:
    item = Item(id=_next_id(), **body.model_dump())
    ITEMS[item.id] = item
    return item


@router.patch("/items/{item_id}", response_model=Item)
def update_item(item_id: int, body: ItemUpdate) -> Item:
    if item_id not in ITEMS:
        raise HTTPException(status_code=404, detail="Item not found")
    existing = ITEMS[item_id]
    updated = existing.model_copy(update=body.model_dump(exclude_unset=True))
    ITEMS[item_id] = updated
    return updated


@router.delete("/items/{item_id}", status_code=204)
def delete_item(item_id: int):
    ITEMS.pop(item_id, None)
    from fastapi.responses import Response
    return Response(status_code=204)


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
