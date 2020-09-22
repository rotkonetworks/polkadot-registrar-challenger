FROM rust:1.45.0 AS builder

RUN apt-get update && apt-get install -y librocksdb-dev clang

WORKDIR /app

COPY . .
RUN cargo build --release

FROM debian:buster-slim

RUN apt-get update && apt-get install -y libssl-dev ca-certificates
RUN update-ca-certificates --fresh

COPY --from=builder /app/target/release/registrar-bot /usr/local/bin

CMD ["/usr/local/bin/registrar-bot"]