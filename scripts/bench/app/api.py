from __future__ import annotations

from fastapi import APIRouter, HTTPException

from .models import Item, ItemCreate, ItemUpdate

router = APIRouter()

_COUNTER: int = 0
ITEMS: dict[int, Item] = {}


def _next_id() -> int:
    global _COUNTER  # noqa: PLW0603
    _COUNTER += 1
    return _COUNTER


def _seed() -> None:
    global _COUNTER  # noqa: PLW0603
    for i in range(1, 11):
        ITEMS[i] = Item(
            id=i,
            name=f"Item {i}",
            description=f"Description for item {i}",
            price=round(i * 9.99, 2),
            tags=[f"tag-{i % 3}", f"tag-{i % 5}"],
        )
        _COUNTER = i


_seed()


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
def delete_item(item_id: int) -> None:
    if item_id not in ITEMS:
        raise HTTPException(status_code=404, detail="Item not found")
    del ITEMS[item_id]
