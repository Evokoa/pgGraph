ARG RUST_IMAGE=docker.io/library/rust:1.96.0-bookworm@sha256:5e2214abe154fe26e39f64488952e5c991eeed1d6d6da7cc8381ae83927f0cfc
ARG POSTGRES_IMAGE=docker.io/library/postgres:17-bookworm@sha256:4f736ae292687621d4dbe0d499ffd024a36bd2ee7d8ca6f2ccd4c800f047b394

FROM ${RUST_IMAGE} AS builder

ARG PG_MAJOR=17
ARG PGRX_VERSION=0.19.1
ARG SFW_VERSION=1.13.1
ARG TARGETARCH

SHELL ["/bin/bash", "-o", "pipefail", "-c"]

RUN apt-get -o Acquire::Retries=3 update \
    && apt-get -o Acquire::Retries=3 install -y --no-install-recommends \
        ca-certificates \
        curl \
        gnupg \
        lsb-release \
    && curl -fsSL https://www.postgresql.org/media/keys/ACCC4CF8.asc \
        | gpg --dearmor -o /usr/share/keyrings/postgresql.gpg \
    && echo "deb [signed-by=/usr/share/keyrings/postgresql.gpg] http://apt.postgresql.org/pub/repos/apt $(lsb_release -cs)-pgdg main" \
        > /etc/apt/sources.list.d/pgdg.list \
    && apt-get -o Acquire::Retries=3 update \
    && apt-get -o Acquire::Retries=3 install -y --no-install-recommends \
        postgresql-${PG_MAJOR} \
        postgresql-server-dev-${PG_MAJOR} \
    && rm -rf /var/lib/apt/lists/*

RUN --mount=type=cache,target=/var/cache/pggraph-sfw \
    case "${TARGETARCH}" in \
        arm64) \
            sfw_asset="sfw-free-linux-arm64"; \
            sfw_sha256="f87bbbca2192fca9740f9bdb115e7cfaa22e957a8f5234d5f97fce1383aa1d66" \
            ;; \
        amd64) \
            sfw_asset="sfw-free-linux-x86_64"; \
            sfw_sha256="4dc46b626a7c5b81c0b54e1984ee53be5a628dbfb2f55ab14e9b04c8a134db6a" \
            ;; \
        *) \
            echo "unsupported Docker architecture for sfw: ${TARGETARCH}" >&2; \
            exit 2 \
            ;; \
    esac \
    && sfw_path="/var/cache/pggraph-sfw/${sfw_asset}-${SFW_VERSION}" \
    && if ! echo "${sfw_sha256}  ${sfw_path}" \
        | sha256sum --check --strict >/dev/null 2>&1; then \
        download_sfw() { \
            curl --http1.1 --fail --silent --show-error --location \
                --connect-timeout 30 --max-time 300 \
                --speed-limit 1024 --speed-time 30 \
                --retry 5 --retry-all-errors --retry-delay 2 \
                "$@" \
                "https://github.com/SocketDev/sfw-free/releases/download/v${SFW_VERSION}/${sfw_asset}" \
                -o "${sfw_path}"; \
        }; \
        if ! download_sfw --continue-at -; then \
            rm -f "${sfw_path}"; \
            download_sfw; \
        fi; \
    fi \
    && echo "${sfw_sha256}  ${sfw_path}" | sha256sum --check --strict \
    && install -m 0755 "${sfw_path}" /usr/local/bin/sfw

ARG CARGO_HTTP_TIMEOUT=120
RUN sfw cargo install cargo-pgrx --version "${PGRX_VERSION}" --locked

WORKDIR /src/graph
COPY graph/ /src/graph/
RUN cargo pgrx init --pg${PG_MAJOR}=/usr/lib/postgresql/${PG_MAJOR}/bin/pg_config \
    && cargo pgrx package --pg-config=/usr/lib/postgresql/${PG_MAJOR}/bin/pg_config

FROM ${POSTGRES_IMAGE}

LABEL org.opencontainers.image.source="https://github.com/evokoa/pggraph" \
      org.opencontainers.image.description="PostgreSQL with pgGraph pre-installed" \
      org.opencontainers.image.licenses="Apache-2.0"

ARG PG_MAJOR=17

RUN apt-get -o Acquire::Retries=3 update \
    && apt-get -o Acquire::Retries=3 install -y --no-install-recommends \
        postgresql-${PG_MAJOR}-cron \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /src/graph/target/release/graph-pg${PG_MAJOR}/usr/share/postgresql/${PG_MAJOR}/extension/graph* /usr/share/postgresql/${PG_MAJOR}/extension/
COPY --from=builder /src/graph/target/release/graph-pg${PG_MAJOR}/usr/lib/postgresql/${PG_MAJOR}/lib/graph.so /usr/lib/postgresql/${PG_MAJOR}/lib/

ENV POSTGRES_DB=graph

COPY docker/init/01-create-extensions-and-schedule.sql /docker-entrypoint-initdb.d/

CMD ["postgres", "-c", "shared_preload_libraries=pg_cron,graph", "-c", "cron.database_name=graph"]
