# A complete extension built on the multi-architecture Husklet React base.
ARG HUSKLET_REACT_IMAGE=ghcr.io/husklet/husklet/extension-react-base:latest
FROM ${HUSKLET_REACT_IMAGE}

WORKDIR /app
COPY --chown=node:node main.js /app/main.js
COPY --chown=node:node extension.toml /etc/husklet/extension.toml

CMD ["node", "/app/main.js"]
LABEL husklet.extension.protocol="1"
LABEL husklet.extension.manifest="/etc/husklet/extension.toml"
