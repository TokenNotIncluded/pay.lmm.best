# Build once with `make package`; the image contains the SAME checked musl binary.
# A multi-platform publish supplies both dist/container/amd64 and /arm64.
FROM scratch
ARG TARGETARCH
ARG VERSION=development
ARG REVISION=unknown
LABEL org.opencontainers.image.title="pay.lmm.best" \
      org.opencontainers.image.description="Payment gateway protocol aggregation only; no payment processing or funds custody" \
      org.opencontainers.image.source="https://github.com/TokenNotIncluded/pay.lmm.best" \
      org.opencontainers.image.licenses="MIT" \
      org.opencontainers.image.version=$VERSION \
      org.opencontainers.image.revision=$REVISION
COPY --chmod=0555 dist/container/${TARGETARCH}/pay-lmm /usr/bin/pay-lmm
COPY --chown=10001:10001 packaging/container-state/ /var/lib/pay-lmm/
USER 10001:10001
WORKDIR /var/lib/pay-lmm
EXPOSE 8080
ENTRYPOINT ["/usr/bin/pay-lmm"]
CMD ["--config", "/etc/pay.lmm.best/config.toml"]
