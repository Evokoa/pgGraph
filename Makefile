EXTENSION = graph
CRATE_DIR = graph
PG_CONFIG ?= pg_config
PGRX ?= cargo pgrx

# Derive the PostgreSQL major from the selected pg_config so the build feature
# matches the target server. pgrx requires the active pgNN feature to match the
# pg_config major, otherwise the default (pg17) is built and install fails on
# any other major.
PG_MAJOR := $(shell $(PG_CONFIG) --version 2>/dev/null | sed -E 's/[^0-9]*([0-9]+).*/\1/')
PG_FEATURE := pg$(PG_MAJOR)

.PHONY: all install installcheck test clean

# pgrx extensions compile and install in a single step via `cargo pgrx install`.
# `make all` is provided for PGXN compatibility but delegates to the same
# install target, because `cargo pgrx package` requires a fully initialised
# pgrx environment that pgxn-client temp directories do not have.
all: install

install:
	cd $(CRATE_DIR) && $(PGRX) install --pg-config $(PG_CONFIG) --release --no-default-features --features $(PG_FEATURE)

installcheck:
	cd $(CRATE_DIR) && $(PGRX) test $(PG_FEATURE)

test: installcheck

clean:
	cd $(CRATE_DIR) && cargo clean
