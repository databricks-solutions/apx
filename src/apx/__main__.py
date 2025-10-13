from typer import Typer
from rich import print

app = Typer(name="apx | project quickstarter")

@app.command(name="init")
def init():
    print("Initializing project...")

if __name__ == "__main__":
    app()