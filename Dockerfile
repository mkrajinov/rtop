# Linux dev/test environment for rtop.
#   make linux-shell   interactive shell (sees the Docker VM's processes via --pid=host)
#   make linux-test    run the test suite on Linux
FROM rust:1-bookworm
RUN apt-get update \
 && apt-get install -y --no-install-recommends procps psmisc iproute2 libcap2-bin lsof strace util-linux \
 && rm -rf /var/lib/apt/lists/*
# make lint inside the container (make linux-shell, then make lint).
RUN rustup component add clippy rustfmt
ENV CARGO_TARGET_DIR=/target
# `rtop` on PATH runs whatever `cargo build --release` last produced.
RUN ln -s /target/release/rtop /usr/local/bin/rtop
WORKDIR /src
