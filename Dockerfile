ARG RUST_IMAGE=rust:1.95.0-bookworm@sha256:503651ea31e66ecb74623beabde781059a5978df1595a9e8ed03974d5fec1bf0
ARG POSTGRES_IMAGE=postgres:17-bookworm

FROM ${RUST_IMAGE} AS builder

ARG PG_MAJOR=17
ARG PGRX_VERSION=0.18.0

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        ca-certificates \
        curl \
        gnupg \
        lsb-release \
    && curl -fsSL https://www.postgresql.org/media/keys/ACCC4CF8.asc \
        | gpg --dearmor -o /usr/share/keyrings/postgresql.gpg \
    && echo "deb [signed-by=/usr/share/keyrings/postgresql.gpg] http://apt.postgresql.org/pub/repos/apt $(lsb_release -cs)-pgdg main" \
        > /etc/apt/sources.list.d/pgdg.list \
    && apt-get update \
    && apt-get install -y --no-install-recommends \
        postgresql-${PG_MAJOR} \
        postgresql-server-dev-${PG_MAJOR} \
    && rm -rf /var/lib/apt/lists/*

RUN cargo install cargo-pgrx --version ${PGRX_VERSION} --locked

WORKDIR /src/graph
COPY graph/ /src/graph/
RUN cargo pgrx init --pg${PG_MAJOR}=/usr/lib/postgresql/${PG_MAJOR}/bin/pg_config \
    && cargo pgrx package --pg-config=/usr/lib/postgresql/${PG_MAJOR}/bin/pg_config

FROM ${POSTGRES_IMAGE}

LABEL org.opencontainers.image.source="https://github.com/evokoa/pggraph" \
      org.opencontainers.image.description="PostgreSQL with pgGraph pre-installed" \
      org.opencontainers.image.licenses="Apache-2.0"

ARG PG_MAJOR=17

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        postgresql-${PG_MAJOR}-cron \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /src/graph/target/release/graph-pg${PG_MAJOR}/usr/share/postgresql/${PG_MAJOR}/extension/graph* /usr/share/postgresql/${PG_MAJOR}/extension/
COPY --from=builder /src/graph/target/release/graph-pg${PG_MAJOR}/usr/lib/postgresql/${PG_MAJOR}/lib/graph.so /usr/lib/postgresql/${PG_MAJOR}/lib/

ENV POSTGRES_DB=graph

COPY docker/init/01-create-extensions-and-schedule.sql /docker-entrypoint-initdb.d/

CMD ["postgres", "-c", "shared_preload_libraries=pg_cron,graph", "-c", "cron.database_name=graph"]
