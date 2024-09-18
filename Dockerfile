FROM debian:trixie-20240904-slim
LABEL mantainer=pando855@gmail.com

ARG CARGO_TARGET_DIR=target
ARG CARGO_BUILD_TARGET=

COPY ${CARGO_TARGET_DIR}/${CARGO_BUILD_TARGET}/release/kaniop /bin/kaniop

ENTRYPOINT ["/bin/kaniop"]
