FROM rust:latest AS builder

WORKDIR /app

RUN rustup target add x86_64-unknown-linux-gnu

COPY ./rust_app ./

RUN cargo build --release --target x86_64-unknown-linux-gnu

FROM ubuntu:24.04 AS runner

# VOLUME [ "/data" ]

COPY --from=builder /app/target/x86_64-unknown-linux-gnu/release/rinha rinha

CMD ["/rinha"]