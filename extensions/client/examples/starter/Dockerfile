# A framework-neutral extension with no React or browser runtime.
ARG NODE_IMAGE=node:22-alpine@sha256:c610fcdfb1d5b4740dd70c284ed3cb16bb857e0f7166196e36a5501df7a3aa32
FROM ${NODE_IMAGE}

WORKDIR /app
COPY --chown=node:node package.json ./
# `npm install` is the explicit host-side preparation step. Copy that exact,
# dependency-free SDK rather than resolving the registry again during the image
# build; this also keeps the installed version and the image version identical.
COPY --chown=node:node node_modules/@husklet/client ./node_modules/@husklet/client
RUN test "$(node -p "require('@husklet/client/package.json').version")" \
    = "$(node -p "require('./package.json').dependencies['@husklet/client']")"
COPY --chown=node:node main.js /app/main.js
COPY --chown=node:node extension.toml /etc/husklet/extension.toml

USER node
CMD ["node", "/app/main.js"]
LABEL husklet.extension.protocol="1"
LABEL husklet.extension.manifest="/etc/husklet/extension.toml"
