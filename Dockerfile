# Image de MCPortal : build Rust en deux étapes, image finale minimale (distroless).
# TLS vers les amonts : rustls (pas d'OpenSSL) ; les certificats racine viennent de l'image.

FROM rust:1.99-slim AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
# benches/ et tests/ ne sont pas nécessaires au binaire, mais Cargo.toml déclare le bench.
COPY benches ./benches
RUN cargo build --release --locked

FROM gcr.io/distroless/cc-debian12:nonroot
COPY --from=build /src/target/release/mcportal /usr/local/bin/mcportal
# Le registre et le guide de déploiement sont montés par Kubernetes (ConfigMap) dans /config.
ENV MCPORTAL_CONFIG=/config/registry.yaml
EXPOSE 8000 9090
ENTRYPOINT ["/usr/local/bin/mcportal"]
