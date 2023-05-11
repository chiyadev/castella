#!/bin/sh
# Starts a local postgres instance. Requires docker.
mkdir -p 'local/postgres'

docker run --rm -it \
  -v "$(pwd)/local/postgres:/var/lib/postgresql/data" \
  -v '/etc/passwd:/etc/passwd:ro' \
  -e 'POSTGRES_USER=chiya' \
  -e 'POSTGRES_PASSWORD=chiya' \
  -e 'POSTGRES_DB=castella' \
  -p '127.0.0.1:5432:5432' \
  --name 'castella-postgres' \
  --user "$(id -u):$(id -g)" \
  'postgres:14-alpine'
