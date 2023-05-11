#!/bin/sh
# Starts a development instance. Assumes postgres was started using `postgres.sh`.
# Required environment variables `CS_OAUTH_CLIENT_ID`, `CS_OAUTH_OAUTH_CLIENT_SECRET` and `CS_OAUTH_REFRESH_TOKEN` should be set in the file `local/env`.
mkdir -p 'local'
touch 'local/env'

export \
  CS_LOG_LEVEL='castella=debug' \
  CS_DB_CONNECTION='postgresql://chiya:chiya@localhost:5432/castella'

set -a; source 'local/env'; set +a
cargo run -- $1
