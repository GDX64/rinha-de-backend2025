FROM rust:latest AS builder

WORKDIR /app

RUN rustup target add x86_64-unknown-linux-gnu

COPY ./rust_app ./

# Use build cache for target directory
RUN --mount=type=cache,target=/app/target,sharing=locked \
  --mount=type=cache,target=/usr/local/cargo/git/db \
  --mount=type=cache,target=/usr/local/cargo/registry/ \
  cargo build --release --bin api --target x86_64-unknown-linux-gnu 

RUN --mount=type=cache,target=/app/target,sharing=locked \
  --mount=type=cache,target=/usr/local/cargo/git/db \
  --mount=type=cache,target=/usr/local/cargo/registry/ \
  cargo build --release --bin db_api --target x86_64-unknown-linux-gnu 

RUN --mount=type=cache,target=/app/target,sharing=locked \
  --mount=type=cache,target=/usr/local/cargo/git/db \
  --mount=type=cache,target=/usr/local/cargo/registry/ \
  cargo build --release --bin redirect --target x86_64-unknown-linux-gnu 

RUN --mount=type=cache,target=/app/target,sharing=locked \
  cp /app/target/x86_64-unknown-linux-gnu/release/api /app/api 

RUN --mount=type=cache,target=/app/target,sharing=locked \
  cp /app/target/x86_64-unknown-linux-gnu/release/redirect /app/redirect 

RUN --mount=type=cache,target=/app/target,sharing=locked \
  cp /app/target/x86_64-unknown-linux-gnu/release/db_api /app/db_api 


FROM ubuntu:24.04 AS runner

COPY --from=builder /app/api /api
COPY --from=builder /app/redirect /redirect
COPY --from=builder /app/db_api /db_api
