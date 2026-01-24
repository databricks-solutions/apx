#!/usr/bin/env python3
"""
Download tokenizer and embedding model for semantic search.

This script downloads the bge-small-en-v1.5 model in ONNX format, which is
a high-quality English sentence embedding model optimized for semantic search
and similarity tasks.

Model details:
- 384-dimensional embeddings
- Max sequence length: 512 tokens
- ONNX fp32 format for efficient inference
- Model size: ~133 MB
"""

import json
import sys
from pathlib import Path
from typing import Optional

try:
    import requests
    from tqdm import tqdm
except ImportError:
    print("Error: Required packages not found.")
    print("Please install: pip install requests tqdm")
    sys.exit(1)


MODEL_ID = "BAAI/bge-small-en-v1.5"
HUGGINGFACE_URL = "https://huggingface.co"


def download_file(url: str, dest_path: Path, desc: Optional[str] = None) -> None:
    """Download a file with progress bar."""
    response = requests.get(url, stream=True)
    response.raise_for_status()

    total_size = int(response.headers.get("content-length", 0))

    with (
        open(dest_path, "wb") as f,
        tqdm(
            desc=desc or dest_path.name,
            total=total_size,
            unit="B",
            unit_scale=True,
            unit_divisor=1024,
        ) as pbar,
    ):
        for chunk in response.iter_content(chunk_size=8192):
            f.write(chunk)
            pbar.update(len(chunk))


def download_model_files(model_id: str, output_dir: Path) -> None:
    """Download model files from Hugging Face."""
    output_dir.mkdir(parents=True, exist_ok=True)

    base_url = f"{HUGGINGFACE_URL}/{model_id}/resolve/main"

    # Files to download
    files_to_download = [
        "tokenizer.json",
        "tokenizer_config.json",
        "config.json",
        "onnx/model.onnx",  # ONNX fp32 format for efficient inference
        "special_tokens_map.json",
        "vocab.txt",
    ]

    print(f"Downloading model: {model_id}")
    print(f"Output directory: {output_dir}")
    print()

    for filename in files_to_download:
        url = f"{base_url}/{filename}"

        # Handle nested paths (e.g., onnx/model.onnx)
        if "/" in filename:
            dest_path = output_dir / filename.replace("/", "_")
        else:
            dest_path = output_dir / filename

        # Skip if file already exists
        if dest_path.exists():
            print(f"✓ {filename} already exists, skipping...")
            continue

        try:
            # Create parent directory if needed
            dest_path.parent.mkdir(parents=True, exist_ok=True)
            download_file(url, dest_path, desc=f"Downloading {filename}")
            print(f"✓ Downloaded {filename}")
        except Exception as e:
            print(f"✗ Failed to download {filename}: {e}")
            # Some files might be optional
            if filename in ["tokenizer.json", "config.json", "onnx/model.onnx"]:
                raise

    print()
    print("✓ All model files downloaded successfully!")
    print(f"  Location: {output_dir.absolute()}")

    # Create a metadata file
    metadata = {
        "model_id": model_id,
        "model_type": "sentence-transformer",
        "embedding_dim": 384,
        "max_seq_length": 512,
        "description": "bge-small-en-v1.5 - High-quality English sentence embeddings (ONNX fp32)",
    }

    metadata_path = output_dir / "model_info.json"
    with open(metadata_path, "w") as f:
        json.dump(metadata, f, indent=2)

    print(f"✓ Created metadata file: {metadata_path.name}")


def main():
    # Get the project root (parent of scripts/)
    script_dir = Path(__file__).parent
    project_root = script_dir.parent

    # Output directory for models
    models_dir = project_root / "assets" / "models"

    # Model-specific directory
    model_dir = models_dir / "bge-small-en-v1.5"

    try:
        download_model_files(MODEL_ID, model_dir)
    except Exception as e:
        print(f"\n✗ Error: {e}", file=sys.stderr)
        sys.exit(1)


if __name__ == "__main__":
    main()
