FROM ghcr.io/rust-cross/manylinux_2_28-cross:x86_64

# Install Rust 1.92 + x86_64-unknown-linux-gnu target
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
    | sh -s -- -y --default-toolchain 1.92.0
ENV PATH="/root/.cargo/bin:${PATH}"
RUN rustup target add x86_64-unknown-linux-gnu

# Install maturin + sccache via pip (fast, pre-built wheels)
RUN pip install --root-user-action=ignore 'maturin>=1.0,<2.0' 'sccache>=0.10.0'

# sccache as default compiler wrapper
ENV RUSTC_WRAPPER=sccache
ENV SCCACHE_DIR=/cache/sccache

WORKDIR /io
