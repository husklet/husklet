FROM husklet/react:latest

WORKDIR /app
COPY src ./src
COPY README.md ./

CMD ["node", "/app/src/main.js"]

LABEL husklet.extension.protocol="1"
COPY extension.toml /etc/husklet/extension.toml
LABEL husklet.extension.manifest="/etc/husklet/extension.toml"
