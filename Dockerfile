FROM rust:latest AS chef
RUN cargo install cargo-chef
WORKDIR app

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
COPY --from=planner /app/recipe.json recipe.json
RUN cargo chef cook --release --recipe-path recipe.json
COPY . .
RUN cargo build --release -p twitch

FROM gcr.io/distroless/cc-debian11 as runner
COPY --from=builder /usr/local/cargo/bin/* /usr/local/bin
EXPOSE 4321
ENTRYPOINT ["/app/twitch"]
