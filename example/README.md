```bash
uv python install 3.12.7 --enable-shared
uv venv
source .venv/bin/activate
uv pip install maturin
PYO3_PYTHON="$(uv python find 3.12)" maturin develop --manifest-path ../python/Cargo.toml --features extension-module
uv run main.py
```
