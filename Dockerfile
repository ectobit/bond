FROM rust:1.60.0 AS builder

WORKDIR /app

RUN cargo install cargo-strip

COPY . /app

RUN cargo build --release && cargo strip

FROM gcr.io/distroless/cc-debian11

COPY --from=builder /app/target/release/bond /

CMD ["./bond"]
