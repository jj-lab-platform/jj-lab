#!/usr/bin/env bash
# Build and push the jjlab runtime image via the shared buildkitd, without
# hitting the public internet.
#
# - jjlab binary:      statically-linked musl (already built on the host)
# - kubectl/helm/buildctl: static CLIs copied from the host's mise tools
# - git + ca-certificates: installed from the alpine base via apk (aliyun mirror)
# - frontend:          embedded in the jjlab binary (no npm stage needed)
#
# The build context is `deploy/chart/context` — build/jjlab and the CLIs are
# copied in by build-image.sh before running this.

set -euo pipefail

REGISTRY="${REGISTRY:-artifact.temp.svc.cluster.local:80}"
TAG="${TAG:-v0.1.0}"
BUILDKIT="${BUILDKIT_ADDR:-tcp://buildkitd.temp.svc.cluster.local:1234}"
DEST="${REGISTRY}/jj-lab:${TAG}"
PROXY="${PROXY:-http://mihomo.develop.svc.cluster.local:7890}"
CTX="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/context"

buildctl --addr "${BUILDKIT}" build \
  --frontend dockerfile.v0 \
  --local "context=${CTX}" \
  --local "dockerfile=${CTX}" \
  --opt "build-arg:HTTP_PROXY=${PROXY}" \
  --opt "build-arg:HTTPS_PROXY=${PROXY}" \
  --opt "build-arg:NO_PROXY=localhost,127.0.0.1,.svc.cluster.local,.svc,.nip.io" \
  --output "type=image,name=${DEST},push=true" \
  --progress plain