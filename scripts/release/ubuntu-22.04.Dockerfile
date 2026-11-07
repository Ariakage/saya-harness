# Build on the project's glibc 2.35 baseline; select target architecture using
# docker build --platform. Supply the architecture-specific base manifest digest;
# the default is the arm64 image used for the recorded P7 build.
ARG BASE_IMAGE=ubuntu@sha256:a8fb2ba88181746111cbbe2969826d6546b71226785b196c93d2c3445a25000d
FROM ${BASE_IMAGE}
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update && apt-get install -y --no-install-recommends build-essential ca-certificates curl pkg-config binutils \
    && rm -rf /var/lib/apt/lists/*
RUN curl --proto '=https' --tlsv1.2 -fsSL https://sh.rustup.rs -o /tmp/rustup.sh \
    && sh /tmp/rustup.sh -y --profile minimal --default-toolchain 1.99.1 \
    && rm /tmp/rustup.sh
ENV PATH=/root/.cargo/bin:$PATH
WORKDIR /src
COPY . .
RUN cargo build --workspace --release --locked
RUN ldd --version | head -n 1 && target/release/saya --version && target/release/saya --help >/dev/null \
    && target/release/saya --config examples/config.example.toml config check
