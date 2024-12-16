FROM rust

WORKDIR /app
COPY . .

ENTRYPOINT cargo run