FROM --platform=$TARGETOS/$TARGETARCH rust:1.97-slim-trixie AS build-image
LABEL org.opencontainers.image.description="AMQP bridge"
LABEL authors="Olegs Korsaks"

ARG TARGETARCH
ARG TARGETOS

WORKDIR /build

RUN apt update && apt install -y --no-install-recommends make musl-tools musl-dev && \
  rm -rf /var/lib/apt/lists/*

COPY ./ /build/

# Map Docker architecture to Rust target
RUN echo "Target architecture is: ${TARGETARCH}" && \
    if [ "${TARGETARCH}" = "amd64" ]; then \
        RUST_TARGETARCH=x86_64 make release; \
    elif [ "${TARGETARCH}" = "arm64" ]; then \
        RUST_TARGETARCH=aarch64 make release; \
    else \
        echo "Unsupported architecture: ${TARGETARCH}"; exit 1; \
    fi

FROM --platform=$TARGETOS/$TARGETARCH gcr.io/distroless/static-debian12:nonroot

LABEL org.opencontainers.image.description="AMQP bridge"
LABEL authors="Olegs Korsaks"

ARG TARGETARCH
ARG TARGETOS

WORKDIR /
COPY --from=build-image /build/target/amqp-bridge /build/LICENSE /

USER nonroot:nonroot

ENTRYPOINT ["/amqp-bridge"]
