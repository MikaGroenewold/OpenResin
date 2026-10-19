# Reproducible Linux build check: tests + desktop app (WebKitGTK 4.1).
# docker build -f tools/docker/linux-build.Dockerfile -t openresin-linux .
FROM rust:1.87-bookworm
RUN apt-get update \
 && apt-get install -y --no-install-recommends libwebkit2gtk-4.1-dev libappindicator3-dev librsvg2-dev \
    patchelf nodejs npm file \
 && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY . .
RUN cd app && npm ci --no-audit --no-fund && npm run build
RUN cargo test --release --workspace --exclude openresin-app
RUN cargo build --release -p openresin-app -p openresin-cli \
 && ls -la target/release/openresin-app target/release/openresin
