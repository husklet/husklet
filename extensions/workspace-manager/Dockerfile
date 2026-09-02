ARG HUSKLET_REACT_IMAGE
FROM ${HUSKLET_REACT_IMAGE}
ARG HUSKLET_EXTENSION_VERSION
ARG HUSKLET_REACT_VERSION

WORKDIR /app
COPY src ./src
COPY README.md ./

USER root
COPY --chown=node:node extension.toml /etc/husklet/extension.toml
RUN test -n "${HUSKLET_EXTENSION_VERSION}" \
    && test -n "${HUSKLET_REACT_VERSION}" \
    && test "$(node -p "require('@husklet/react/package.json').version")" = "${HUSKLET_REACT_VERSION}" \
    && sed -i "s/^version = .*/version = \"${HUSKLET_EXTENSION_VERSION}\"/" /etc/husklet/extension.toml
USER node

CMD ["node", "/app/src/main.js"]

LABEL husklet.extension.protocol="1"
LABEL org.opencontainers.image.version="${HUSKLET_EXTENSION_VERSION}"
LABEL husklet.extension.manifest="/etc/husklet/extension.toml"
