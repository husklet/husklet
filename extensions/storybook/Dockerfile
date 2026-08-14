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
LABEL husklet.extension.manifest="{\"name\":\"storybook\",\"display_name\":\"Component playground\",\"version\":\"0.1.0\",\"protocol\":1,\"capabilities\":[\"interface\"],\"activation\":\"manual\",\"interface\":{\"tab_title\":\"Storybook\"}}"
