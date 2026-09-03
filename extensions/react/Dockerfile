# The base image every React extension builds FROM.
#
# An extension adds its own source and its own manifest label; everything it
# needs to reach the host — the runtime, this package, React — is already here.

ARG NODE_IMAGE=node:22-alpine@sha256:c610fcdfb1d5b4740dd70c284ed3cb16bb857e0f7166196e36a5501df7a3aa32
FROM ${NODE_IMAGE} AS package
ARG HUSKLET_REACT_VERSION
ARG NODE_VERSION=22.23.2
ARG NPM_VERSION=10.9.8
WORKDIR /source
COPY client ./client
COPY react ./react
RUN test "$(node --version)" = "v${NODE_VERSION}" \
    && test "$(npm --version)" = "${NPM_VERSION}" \
    && test -n "${HUSKLET_REACT_VERSION}" \
    && npm pkg set "version=${HUSKLET_REACT_VERSION}" --prefix client \
    && npm pack --ignore-scripts --pack-destination /out --prefix client \
    && npm pkg set "version=${HUSKLET_REACT_VERSION}" --prefix react \
    && npm pkg set "dependencies.@husklet/client=${HUSKLET_REACT_VERSION}" --prefix react \
    && sed -i "s/^version = .*/version = \"${HUSKLET_REACT_VERSION}\"/" react/examples/starter/extension.toml \
    && npm pack --ignore-scripts --pack-destination /out --prefix react

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
COPY react/image-runtime/package.json react/image-runtime/package-lock.json ./
RUN test "$(node --version)" = "v${NODE_VERSION}" \
    && test "$(npm --version)" = "${NPM_VERSION}" \
    && npm ci --ignore-scripts --omit=dev --no-audit --no-fund \
    && mkdir -p node_modules/@husklet/client node_modules/@husklet/react \
    && tar -xzf /opt/husklet-sdk/husklet-client-*.tgz --strip-components=1 -C node_modules/@husklet/client \
    && tar -xzf /opt/husklet-sdk/husklet-react-*.tgz --strip-components=1 -C node_modules/@husklet/react \
    && test "$(node -p "require('@husklet/client/package.json').version")" = "${HUSKLET_REACT_VERSION}" \
    && test "$(node -p "require('@husklet/react/package.json').version")" = "${HUSKLET_REACT_VERSION}" \
    && npm cache clean --force

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
