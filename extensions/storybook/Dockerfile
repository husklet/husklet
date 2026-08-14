# The playground, built on the React base image.
#
# Everything needed to reach the host — the runtime, @husklet/react, React —
# comes from the base; this image adds source and its own manifest.

FROM husklet/react:latest

WORKDIR /app
COPY src ./src
COPY README.md ./

CMD ["node", "/app/src/main.js"]

# The manifest the host reads off the image. The fields are those of
# hl_extension::manifest::Manifest; unknown fields are refused, not ignored.
LABEL husklet.extension.protocol="1"
COPY extension.toml /etc/husklet/extension.toml
# The label names where the manifest lives; the host reads the file.
LABEL husklet.extension.manifest="/etc/husklet/extension.toml"
