# syntax=docker/dockerfile:1.3
FROM rust:1.60.0 AS builder

ARG TARGETPLATFORM

WORKDIR /app

RUN cargo install cargo-strip

COPY . /app

RUN --mount=type=cache,target=/usr/local/cargo/registry,id=${TARGETPLATFORM} --mount=type=cache,target=/app/target,id=${TARGETPLATFORM} cargo build --release && cargo strip

FROM gcr.io/distroless/cc-debian11

COPY --from=builder /app/target/release/bond /

CMD ["./bond"]
