# syntax=docker/dockerfile:1
# jjlab runtime image — independent build, no public internet.
#
# Everything is staged locally (see build-image.sh): the statically-linked
# jjlab binary, the static kubectl/helm/buildctl CLIs, and git comes from the
# alpine base via apk. The frontend is embedded in the jjlab binary at compile
# time, so no npm/node stage is needed here. Apk runs through the ambient
# HTTP proxy (passed as build-args) to mirrors.aliyun.com.
FROM artifact.temp.svc.cluster.local:80/library/alpine:3
ARG HTTP_PROXY
ARG HTTPS_PROXY
ARG NO_PROXY=localhost,127.0.0.1,.svc.cluster.local,.svc,.nip.io
ENV HTTP_PROXY=${HTTP_PROXY} \
    HTTPS_PROXY=${HTTPS_PROXY} \
    NO_PROXY=${NO_PROXY}
RUN sed -i 's|dl-cdn.alpinelinux.org|mirrors.aliyun.com|g' /etc/apk/repositories \
    && apk add --no-cache ca-certificates git
COPY build/jjlab /opt/jjlab/jjlab
COPY build/buildctl /usr/local/bin/buildctl
COPY build/kubectl /usr/local/bin/kubectl
COPY build/helm /usr/local/bin/helm
ENV JJLAB_PORT=8080 \
    JJLAB_DB=/data/data.db \
    JJLAB_REPOS=/data/repos \
    JJLAB_ASSETS=/data/assets \
    JJLAB_LOGS=/data/logs \
    JJLAB_CI_NAMESPACE=temp \
    JJLAB_BUILDKIT_ADDR=tcp://buildkitd.temp.svc.cluster.local:1234 \
    JJLAB_CI_IMAGE=artifact.temp.svc.cluster.local/library/alpine:3
EXPOSE 8080
ENTRYPOINT ["/opt/jjlab/jjlab"]