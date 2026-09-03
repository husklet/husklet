# A framework-neutral extension with no React or browser runtime.
ARG NODE_IMAGE=node:22-alpine@sha256:c610fcdfb1d5b4740dd70c284ed3cb16bb857e0f7166196e36a5501df7a3aa32
FROM ${NODE_IMAGE}

WORKDIR /app
COPY --chown=node:node package*.json ./
RUN npm ci --ignore-scripts --omit=dev --no-audit --no-fund \
    && npm cache clean --force
COPY --chown=node:node main.js /app/main.js
COPY --chown=node:node extension.toml /etc/husklet/extension.toml

USER node
CMD ["node", "/app/main.js"]
LABEL husklet.extension.protocol="1"
LABEL husklet.extension.manifest="/etc/husklet/extension.toml"
