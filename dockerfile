FROM rust:latest AS builder

WORKDIR /app

RUN rustup target add x86_64-unknown-linux-gnu

COPY ./rust_app ./

# Use build cache for target directory
RUN --mount=type=cache,target=/app/target,sharing=locked \
  cargo build --release --target x86_64-unknown-linux-gnu 

RUN --mount=type=cache,target=/app/target,sharing=locked \
  cp /app/target/x86_64-unknown-linux-gnu/release/rinha /app/rinha 


CMD ["ls"]

FROM ubuntu:24.04 AS runner

COPY --from=builder /app/rinha /rinha

CMD ["/rinha"]