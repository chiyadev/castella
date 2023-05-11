FROM rust:alpine AS build
WORKDIR /build

RUN apk add --no-cache musl-dev
RUN cargo init --bin --name castella

COPY Cargo.toml Cargo.lock ./
RUN cargo fetch

COPY . .
RUN cargo build --release
RUN strip target/release/castella

FROM alpine
WORKDIR /castella

RUN apk add --no-cache tini
COPY --from=build /build/target/release/castella /usr/local/bin/castella

RUN addgroup -S castella -g 1000 && adduser -S castella -G castella -h /castella -u 1000
USER castella

EXPOSE 1707/tcp
ENV CS_SERVER_ENDPOINT="0.0.0.0:1707"
ENTRYPOINT ["/sbin/tini", "--", "/usr/local/bin/castella"]
