#!/bin/sh
# Connects to a local postgres instance. Assumes it was started using `postgres.sh`.
docker exec -it 'castella-postgres' \
  psql 'postgresql://chiya:chiya@localhost:5432/castella'
