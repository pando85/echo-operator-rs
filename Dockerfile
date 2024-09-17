ARG BASE_IMAGE=rust:1.81.0
FROM ${BASE_IMAGE} AS builder
LABEL mantainer pando855@gmail.com

WORKDIR /usr/src/kaniop
COPY . .
RUN cargo build --locked --release --bin kaniop

FROM debian:trixie-20240904-slim
LABEL mantainer pando855@gmail.com

COPY --from=builder /usr/src/kaniop/target/release/kaniop /bin/kaniop

ENTRYPOINT [ "/bin/kaniop" ]
