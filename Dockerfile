# MCPortal image: two-stage Rust build, minimal final image (distroless).
# TLS to upstreams: rustls (no OpenSSL); root certificates come from the image.

# Both stages must use the same Debian release: the binary links against the build stage's
# glibc, so a newer build base than the runtime base fails at startup with
# "GLIBC_x.yy not found". Bump them together.
FROM rust:1.99-slim-trixie AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
# benches/ and tests/ are not needed for the binary, but Cargo.toml declares the bench.
COPY benches ./benches
RUN cargo build --release --locked

FROM gcr.io/distroless/cc-debian13:nonroot
COPY --from=build /src/target/release/mcportal /usr/local/bin/mcportal
# The registry and the deployment guide are mounted by Kubernetes (ConfigMap) into /config.
ENV MCPORTAL_CONFIG=/config/registry.yaml
EXPOSE 8000 9090
ENTRYPOINT ["/usr/local/bin/mcportal"]
