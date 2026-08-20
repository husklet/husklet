# The base image every React extension builds FROM.
#
# An extension adds its own source and its own manifest label; everything it
# needs to reach the host — the runtime, this package, React — is already here.

FROM node:22-alpine

# The package lives inside the working directory's own module tree, because ESM
# resolution walks directories and ignores NODE_PATH entirely.
# React is a peer dependency of the package — an extension and the renderer
# must share one copy, or every hook call fails — so this image, which is that
# shared environment, installs it at the root where both resolve to it.
#
# It is installed before the package is placed, because installing afterwards
# prunes anything npm did not put there itself.
WORKDIR /app
RUN npm install --no-audit --no-fund --save-exact react@18.3.1 react-reconciler@0.29.2

# The package lives inside the working directory's own module tree, because ESM
# resolution walks directories and ignores NODE_PATH entirely.
WORKDIR /app/node_modules/@husklet/react
COPY package.json ./
COPY src ./src
COPY README.md ./

WORKDIR /app
ENV NODE_ENV=production

# What an extension overrides. The host starts the image as it is, so a base
# with no source of its own says so rather than failing obscurely.
CMD ["node", "--input-type=module", "-e", "console.error('this image is a base; an extension supplies its own entrypoint'); process.exit(64)"]

# The manifest the host reads off the image. An extension replaces this label
# with its own; the fields are those of hl_extension::manifest::Manifest.
LABEL husklet.extension.protocol="1"
# A base image declares no manifest: it is not an extension, it is what
# extensions are built from. Each of those copies its own extension.toml
# and points this label at it.
