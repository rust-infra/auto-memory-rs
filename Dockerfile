# syntax=docker/dockerfile:1
#
# `auto-memory` with semantic search working out of the box.
#
# The release archives deliberately ship no ONNX Runtime (the crate builds with `ort`'s
# `load-dynamic`, so the binary resolves `libonnxruntime` at run time). That trade is
# right for a host install — the artifact stays portable and semantic search degrades to
# a warning — and wrong inside a container, where the image *is* the environment. So this
# image bundles both halves the archives leave out: the ONNX Runtime shared library and
# the quantized embedding model cache. Nothing is downloaded at run time.
#
# The reference golden vectors were produced with onnxruntime 1.29.0
# (`docs/reference.md` §6f), and the image bundles that same wheel, so the container
# matches the version the captures came from and needs no cross-version drift warning.
#
# Each architecture is built on a runner of that architecture (see
# `.github/workflows/container.yml`), so `TARGETPLATFORM` needs no handling here: the
# Rust build and the pip wheel are both native.

# --- the binary -------------------------------------------------------------
FROM rust:bookworm AS builder
WORKDIR /src
# The manifest first and the sources second, so editing a source file does not invalidate
# the dependency download layer. Tests, docs and `target/` are excluded by
# `.dockerignore` — none of them are inputs to `cargo build --release`.
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release --locked

# --- ONNX Runtime and the model ---------------------------------------------
FROM python:3.13-slim AS runtime-assets

# `--no-deps` because this stage exists to extract one file: the wheel's own
# dependencies (numpy and friends) would be dead weight. The only version that matters
# is the one in the filename.
RUN pip install --no-cache-dir --no-deps onnxruntime==1.29.0 \
 && mkdir -p /opt/onnxruntime \
 && cp -a /usr/local/lib/python3*/site-packages/onnxruntime/capi/libonnxruntime.so* /opt/onnxruntime/ \
 && cd /opt/onnxruntime \
 && ln -s "$(basename "$(ls libonnxruntime.so.1.* | head -1)")" libonnxruntime.so

# The embedding model, written in the huggingface-hub cache layout the Rust side walks
# (`models--<repo>/snapshots/<revision>/model_optimized.onnx`). Plain `snapshot_download`
# rather than fastembed's own loader so the layout is the hub's and the revision lands in
# `refs/main`; `allow_patterns` keeps the full-precision `model.onnx` (which the port
# never reads, it uses the quantized export) out of the image.
RUN pip install --no-cache-dir huggingface_hub==1.32.0
RUN python - <<'PY'
import shutil

from huggingface_hub import snapshot_download

snapshot_download(
    repo_id="qdrant/bge-small-en-v1.5-onnx-q",
    cache_dir="/models",
    allow_patterns=[
        "model_optimized.onnx",
        "config.json",
        "tokenizer.json",
        "tokenizer_config.json",
        "special_tokens_map.json",
    ],
)
# Only ever held by the download itself.
shutil.rmtree("/models/.locks", ignore_errors=True)
PY

# --- runtime ----------------------------------------------------------------
FROM debian:bookworm-slim

LABEL org.opencontainers.image.title="auto-memory-rs" \
      org.opencontainers.image.description="Local-first markdown memory index: CLI plus streamable-HTTP MCP server, with semantic search bundled" \
      org.opencontainers.image.licenses="AGPL-3.0-or-later" \
      org.opencontainers.image.source="https://github.com/rust-infra/auto-memory-rs"

COPY --from=builder /src/target/release/auto-memory /usr/local/bin/auto-memory
# `/usr/local/lib` is already on the runtime search path, but naming the library keeps
# `doctor` from having to guess among the copies the wheel ships.
COPY --from=runtime-assets /opt/onnxruntime /usr/local/lib/
COPY --from=runtime-assets /models /opt/auto-memory/models
RUN ldconfig

ENV ORT_DYLIB_PATH=/usr/local/lib/libonnxruntime.so
ENV AUTO_MEMORY_MODEL_CACHE=/opt/auto-memory/models

# The image bundles the same ONNX Runtime the reference captures were produced with
# (1.29.0), so there is no version drift to warn about; the entrypoint stays as the
# single place a future warning would go.
RUN cat > /usr/local/bin/auto-memory-entrypoint <<'EOF'
#!/bin/sh
exec auto-memory "$@"
EOF
RUN chmod +x /usr/local/bin/auto-memory-entrypoint

# `/vault` holds the markdown notes and `/index` the derived SQLite index; both are meant
# to be bind-mounted. Declaring them keeps `docker run` without `-v` from quietly writing
# into the container's writable layer.
VOLUME ["/vault", "/index"]

# `/index` is a directory, so a bind mount may arrive without the database in it yet:
# `Store::open` creates it, and the project is registered on the first run.
ENTRYPOINT ["auto-memory-entrypoint"]
# The HTTP transport rather than stdio, because a container is a long-running server;
# stdio would need `docker run -i` and one client attached to that process. Add
# `--read-only` to serve without writing back to the vault.
#
# `--model-cache` is load-bearing, not decoration: the MCP command only builds the
# embedding provider when one of `--model-cache` / `--onnx-runtime` /
# `--embedding-fixture` is passed, so `AUTO_MEMORY_MODEL_CACHE` alone would leave
# semantic search reporting itself unavailable.
CMD ["mcp", "--vault", "/vault", "--index", "/index/memory.db", \
     "--model-cache", "/opt/auto-memory/models", \
     "--http", "--host", "0.0.0.0", "--port", "8765"]
