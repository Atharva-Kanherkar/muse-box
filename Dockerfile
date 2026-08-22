# One image, one origin. The client is served by the backend so the session
# cookie is first-party: a separate frontend deployment would be cross-site,
# and browsers are actively dropping cross-site cookies.

FROM node:22-alpine AS client
WORKDIR /client
COPY web/package.json web/package-lock.json ./
RUN npm ci
COPY web/ ./
RUN npm run build

FROM rust:1-alpine AS server
RUN apk add --no-cache musl-dev
WORKDIR /server
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY testing ./testing
RUN cargo build --release --locked

FROM alpine:3.20
RUN apk add --no-cache ca-certificates
WORKDIR /app
COPY --from=server /server/target/release/muse-box ./muse-box
COPY --from=client /client/dist ./web/dist
ENV CLIENT_ROOT=/app/web/dist
CMD ["./muse-box"]
