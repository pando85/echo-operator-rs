FROM debian:trixie-20240904-slim
LABEL mantainer=pando855@gmail.com

COPY target/release/kaniop /bin/kaniop

ENTRYPOINT ["/bin/kaniop"]
