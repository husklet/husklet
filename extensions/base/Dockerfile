# The base image every TypeScript extension builds FROM.
#
# An extension adds its own source and its own manifest label; everything it
# needs to reach the host — the runtime, this package, React — is already here.

ARG NODE_IMAGE=node:22-bookworm-slim@sha256:83f487e0a63425e5b4d146fb5e5be574bcbe1b7b843d3ebafdd95eaf7767a7e5
FROM ${NODE_IMAGE} AS package
ARG HUSKLET_REACT_VERSION
ARG NODE_VERSION=22.23.2
ARG NPM_VERSION=10.9.8
WORKDIR /source
COPY packages/client ./client
COPY packages/react ./react
RUN test -n "${HUSKLET_REACT_VERSION}"
RUN node -e "if (process.version !== 'v${NODE_VERSION}') process.exit(1); if (require('/usr/local/lib/node_modules/npm/package.json').version !== '${NPM_VERSION}') process.exit(1)"
RUN npm --prefix client pkg set "version=${HUSKLET_REACT_VERSION}" \
    && npm --prefix react pkg set "version=${HUSKLET_REACT_VERSION}" \
    && npm --prefix react pkg set "dependencies.@husklet/client=${HUSKLET_REACT_VERSION}" \
    && sed -i "s/^version = .*/version = \"${HUSKLET_REACT_VERSION}\"/" react/examples/starter/extension.toml
RUN mkdir -p /out/client /out/react \
    && (cd client && npm pack --ignore-scripts --pack-destination /out/client) \
    && (cd react && npm pack --ignore-scripts --pack-destination /out/react)

FROM ${NODE_IMAGE}
ARG NODE_IMAGE
ARG HUSKLET_REACT_VERSION
ARG NODE_VERSION=22.23.2
ARG NPM_VERSION=10.9.8

# The package lives inside the working directory's own module tree, because ESM
# resolution walks directories and ignores NODE_PATH entirely.
# React is a peer dependency of the package — an extension and the renderer
# must share one copy, or every hook call fails — so this image, which is that
# shared environment, installs it at the root where both resolve to it.
#
# It is installed before the package is placed, because installing afterwards
# prunes anything npm did not put there itself.
WORKDIR /app
COPY --from=package /out /opt/husklet-sdk
COPY extensions/base/package.json extensions/base/package-lock.json ./
RUN node -e "if (process.version !== 'v${NODE_VERSION}') process.exit(1); if (require('/usr/local/lib/node_modules/npm/package.json').version !== '${NPM_VERSION}') process.exit(1)"
RUN npm install --global --ignore-scripts --no-audit --no-fund \
       /opt/husklet-sdk/client/husklet-client-*.tgz \
       /opt/husklet-sdk/react/husklet-react-*.tgz \
       react@18.3.1 react-reconciler@0.29.2 \
       typescript@5.9.2 vite@7.1.4 @types/node@22.20.1 @types/react@18.3.3 \
    && mkdir -p node_modules/@husklet node_modules/@types \
    && ln -s "$(npm root --global)/@husklet/client" node_modules/@husklet/client \
    && ln -s "$(npm root --global)/@husklet/react" node_modules/@husklet/react \
    && ln -s "$(npm root --global)/react" node_modules/react \
    && ln -s "$(npm root --global)/react-reconciler" node_modules/react-reconciler \
    && ln -s "$(npm root --global)/typescript" node_modules/typescript \
    && ln -s "$(npm root --global)/vite" node_modules/vite \
    && ln -s "$(npm root --global)/@types/node" node_modules/@types/node \
    && ln -s "$(npm root --global)/@types/react" node_modules/@types/react \
    && mkdir -p /run/husklet \
    && touch /run/husklet/extension.sock \
    && npm cache verify
RUN node -e "if (require('@husklet/client/package.json').version !== '${HUSKLET_REACT_VERSION}') process.exit(1); if (require('@husklet/react/package.json').version !== '${HUSKLET_REACT_VERSION}') process.exit(1)"

WORKDIR /app
ENV NODE_ENV=production
ENV HUSKLET_EXTENSION_SOCKET=/run/husklet/extension.sock
USER node

# What an extension overrides. The host starts the image as it is, so a base
# with no source of its own says so rather than failing obscurely.
CMD ["node", "--input-type=module", "-e", "console.error('this image is a base; an extension supplies its own entrypoint'); process.exit(64)"]

# The manifest the host reads off the image. An extension replaces this label
# with its own; the fields are those of hl_extension::manifest::Manifest.
LABEL husklet.extension.protocol="1"
LABEL husklet.extension.node.version="${NODE_VERSION}"
LABEL husklet.extension.npm.version="${NPM_VERSION}"
LABEL org.opencontainers.image.base.name="${NODE_IMAGE}"
LABEL org.opencontainers.image.version="${HUSKLET_REACT_VERSION}"
LABEL org.opencontainers.image.source="https://github.com/husklet/husklet"
# A base image declares no manifest: it is not an extension, it is what
# extensions are built from. Each of those copies its own extension.toml
# and points this label at it.
