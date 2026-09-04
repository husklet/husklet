ARG HUSKLET_REACT_IMAGE
FROM ${HUSKLET_REACT_IMAGE} AS build
USER root
RUN npm install --no-save --ignore-scripts --no-audit --no-fund \
    typescript@5.9.2 vite@7.1.4 @types/node@22.20.1 @types/react@18.3.3
COPY src ./src
COPY tsconfig.json vite.config.ts ./
RUN tsc --noEmit && vite build

FROM ${HUSKLET_REACT_IMAGE}
ARG HUSKLET_EXTENSION_VERSION
ARG HUSKLET_REACT_VERSION

WORKDIR /app
COPY --from=build /app/dist ./dist
USER root
COPY --chown=node:node extension.toml /etc/husklet/extension.toml
RUN test -n "${HUSKLET_EXTENSION_VERSION}" \
    && test -n "${HUSKLET_REACT_VERSION}" \
    && test "$(node -p "require('@husklet/client/package.json').version")" = "${HUSKLET_REACT_VERSION}" \
    && test "$(node -p "require('@husklet/react/package.json').version")" = "${HUSKLET_REACT_VERSION}" \
    && sed -i "s/^version = .*/version = \"${HUSKLET_EXTENSION_VERSION}\"/" /etc/husklet/extension.toml
USER node

CMD ["node", "/app/dist/main.js"]

LABEL husklet.extension.protocol="1"
LABEL org.opencontainers.image.version="${HUSKLET_EXTENSION_VERSION}"
LABEL org.opencontainers.image.base.name="${HUSKLET_REACT_IMAGE}"
LABEL org.opencontainers.image.source="https://github.com/husklet/husklet"
LABEL husklet.extension.manifest="/etc/husklet/extension.toml"
