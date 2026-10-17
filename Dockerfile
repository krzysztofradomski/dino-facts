FROM rust:1-alpine AS builder
RUN apk add --no-cache musl-dev
WORKDIR /app
COPY Cargo.toml ./
COPY src ./src
RUN cargo build --release

FROM alpine:3.21
RUN apk add --no-cache ca-certificates
COPY --from=builder /app/target/release/dino-trivia /usr/local/bin/dino-trivia
USER nobody
EXPOSE 8080
CMD ["dino-trivia"]
