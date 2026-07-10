FROM rust:1.93-slim-trixie AS builder

RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates curl \
 && rm -rf /var/lib/apt/lists/*

RUN rustup target add wasm32-unknown-unknown \
 && curl -sSf https://rustwasm.github.io/wasm-pack/installer/init.sh | sh

WORKDIR /build
COPY . .

RUN wasm-pack build --target web --out-dir www/pkg web

RUN COOPER_SKIP_WASM_BUILD=1 cargo build --release --locked -p agent-cooper \
 && cp target/release/cooper /usr/local/bin/cooper

FROM debian:trixie-slim

RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --create-home cooper

COPY --from=builder /usr/local/bin/cooper /usr/local/bin/cooper
COPY --from=builder /build/web/www /opt/cooper/web/www

USER cooper
WORKDIR /home/cooper

EXPOSE 8080

CMD ["sh", "-c", "cooper web --host 0.0.0.0 --port ${PORT:-8080} --dir /opt/cooper/web"]
