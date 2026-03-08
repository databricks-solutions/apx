from pydantic import BaseModel


class ItemCreate(BaseModel):
    name: str
    description: str | None = None
    price: float
    tags: list[str] = []


class ItemUpdate(BaseModel):
    name: str | None = None
    description: str | None = None
    price: float | None = None
    tags: list[str] | None = None


class Item(BaseModel):
    id: int
    name: str
    description: str | None = None
    price: float
    tags: list[str] = []
