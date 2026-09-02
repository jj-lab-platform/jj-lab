# syntax=docker/dockerfile:1
# jjlab: single static musl binary with an embedded Svelte SPA, plus the
# tool CLIs (git, kubectl, helm, buildctl) needed by the k8s-native CI.
#
# The build stages mirror the historical jjlab Dockerfile: frontend (pnpm),
# rust (musl via cargo), then an alpine runtime that layers in the CLIs.
# Base images come from the in-cluster artifact registry; crate/npm deps go
# through the in-cluster indexes, so the build never reaches the public net.
ARG REGISTRY=jj-lab.temp.10.199.64.20.nip.io
ARG RUST_IMAGE=1.97.1-alpine3.24
ARG NODE_IMAGE=22-alpine

# ---- frontend ----
FROM ${REGISTRY}/library/node:${NODE_IMAGE} AS frontend
ARG HTTP_PROXY=http://mihomo.develop.svc.cluster.local:789
ARG HTTPS_PROXY=http://mihomo.develop.svc.cluster.local:789
ENV HTTP_PROXY=${HTTP_PROXY} \
    HTTPS_PROXY=${HTTPS_PROXY} \
    NO_PROXY=localhost,127.0.0.1,.svc.cluster.local,.svc,.nip.io,10.199.64.20,.develop.10.199.64.20.nip.io
WORKDIR /fe
COPY frontend/package.json frontend/pnpm-lock.yaml ./
RUN corepack enable && pnpm install --frozen-lockfile || pnpm install
COPY frontend/ ./
RUN pnpm build

# ---- rust build ----
FROM ${REGISTRY}/library/rust:${RUST_IMAGE} AS build
ARG HTTP_PROXY=http://mihomo.develop.svc.cluster.local:789
ARG HTTPS_PROXY=http://mihomo.develop.svc.cluster.local:789
ENV HTTP_PROXY=${HTTP_PROXY} \
    HTTPS_PROXY=${HTTPS_PROXY} \
    NO_PROXY=localhost,127.0.0.1,.svc.cluster.local,.svc,.nip.io,10.199.64.20,.develop.10.199.64.20.nip.io \
    CARGO_HOME=/root/.cargo
RUN sed -i 's|dl-cdn.alpinelinux.org|mirrors.aliyun.com|g' /etc/apk/repositories \
    && apk add --no-cache build-base \
    && rustup target add x86_64-unknown-linux-musl
WORKDIR /build
ENV CARGO_REGISTRIES_CRATES_IO_PROTOCOL=sparse \
    CARGO_REGISTRIES_CRATES_IO_INDEX=${REGISTRY}/pkgs/cargo/index/
COPY Cargo.toml Cargo.lock ./
RUN mkdir -p src && printf 'fn main() {}\n' > src/main.rs
RUN --mount=type=cache,target=/root/.cargo \
    cargo fetch --target x86_64-unknown-linux-musl
COPY crates ./crates
COPY --from=frontend /dist ./dist
RUN --mount=type=cache,target=/root/.cargo \
    --mount=type=cache,target=/build/target \
    cargo build --release --locked --target x86_64-unknown-linux-musl \
    && cp target/x86_64-unknown-linux-musl/release/jjlab /out

# ---- runtime ----
# git (smart-HTTP) + kubectl/helm/buildctl (k8s-native CI CLIs). kubectl is
# only a convenience/ops tool — the scheduler talks to k8s via the `kube`
# crate; helm/buildctl are spawned as subprocesses exactly like git. buildctl
# targets the shared buildkitd at JJLAB_BUILDKIT_ADDR.
FROM ${REGISTRY}/library/alpine:3.24
RUN sed -i 's|dl-cdn.alpinelinux.org|mirrors.aliyun.com|g' /etc/apk/repositories \
    && apk add --no-cache ca-certificates git kubectl helm buildctl
COPY --from=build /out /usr/local/bin/jjlab
ENV JJLAB_PORT=8080 \
    JJLAB_DB=/data/data.db \
    JJLAB_REPOS=/data/repos \
    JJLAB_ASSETS=/data/assets \
    JJLAB_LOGS=/data/logs \
    JJLAB_CI_NAMESPACE=temp \
    JJLAB_BUILDKIT_ADDR=tcp://buildkitd.temp.svc.cluster.local:1234 \
    JJLAB_CI_IMAGE=jj-lab.temp.10.199.64.20.nip.io/library/alpine:3
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/jjlab"]