# The playground, built on the React base image.
#
# Everything needed to reach the host — the runtime, @husklet/react, React —
# comes from the base; this image adds source and its own manifest.

ARG HUSKLET_REACT_IMAGE
FROM ${HUSKLET_REACT_IMAGE} AS build
USER root
RUN npm install --no-save --ignore-scripts --no-audit --no-fund \
    typescript@5.9.2 @types/node@22.20.1 @types/react@18.3.3
COPY src ./src
COPY tsconfig.json ./
RUN tsc --project tsconfig.json

FROM ${HUSKLET_REACT_IMAGE}
ARG HUSKLET_EXTENSION_VERSION
ARG HUSKLET_REACT_VERSION

WORKDIR /app
COPY --from=build /app/dist ./dist
COPY README.md ./

USER root
COPY --chown=node:node extension.toml /etc/husklet/extension.toml
RUN test -n "${HUSKLET_EXTENSION_VERSION}" \
    && test -n "${HUSKLET_REACT_VERSION}" \
    && test "$(node -p "require('@husklet/client/package.json').version")" = "${HUSKLET_REACT_VERSION}" \
    && test "$(node -p "require('@husklet/react/package.json').version")" = "${HUSKLET_REACT_VERSION}" \
    && sed -i "s/^version = .*/version = \"${HUSKLET_EXTENSION_VERSION}\"/" /etc/husklet/extension.toml
USER node

CMD ["node", "/app/dist/main.js"]

# The manifest the host reads off the image. The fields are those of
# hl_extension::manifest::Manifest; unknown fields are refused, not ignored.
LABEL husklet.extension.protocol="1"
LABEL org.opencontainers.image.version="${HUSKLET_EXTENSION_VERSION}"
LABEL org.opencontainers.image.base.name="${HUSKLET_REACT_IMAGE}"
LABEL org.opencontainers.image.source="https://github.com/husklet/husklet"
# The label names where the manifest lives; the host reads the file.
LABEL husklet.extension.manifest="/etc/husklet/extension.toml"
