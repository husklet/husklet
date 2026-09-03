#!/usr/bin/env bash
set -euo pipefail

image="${1:?image is required}"
version="${2:?version is required}"
kind="${3:?base or extension name is required}"
architecture="${4:?amd64 or arm64 is required}"
platform="linux/$architecture"

fail() {
  echo "extension image smoke: $*" >&2
  exit 1
}

inspect() {
  docker image inspect --format "$1" "$image"
}

[[ "$(inspect '{{.Architecture}}')" == "$architecture" ]] || fail "$image is not $platform"
[[ "$(inspect '{{.Os}}')" == linux ]] || fail "$image is not Linux"
[[ "$(inspect '{{.Config.User}}')" == node ]] || fail "$image does not run as node"
[[ "$(inspect '{{index .Config.Labels "husklet.extension.protocol"}}')" == 1 ]] \
  || fail "$image does not declare protocol 1"
[[ "$(inspect '{{index .Config.Labels "org.opencontainers.image.version"}}')" == "$version" ]] \
  || fail "$image version label does not match $version"
node_version="$(inspect '{{index .Config.Labels "husklet.extension.node.version"}}')"
npm_version="$(inspect '{{index .Config.Labels "husklet.extension.npm.version"}}')"
[[ "$node_version" == 22.23.2 ]] || fail "$image does not carry the pinned Node version"
[[ "$npm_version" == 10.9.8 ]] || fail "$image does not carry the pinned npm version"
case "$(inspect '{{json .Config.Env}}')" in
  *'"HUSKLET_EXTENSION_SOCKET=/run/husklet/extension.sock"'*) ;;
  *) fail "$image omits the extension socket environment" ;;
esac

if [[ "$kind" == base ]]; then
  [[ "$(inspect '{{index .Config.Labels "husklet.extension.manifest"}}')" == '<no value>' ]] \
    || fail "the base image must not advertise an extension manifest"
  docker run --rm --platform "$platform" --entrypoint node \
    -e EXPECTED_VERSION="$version" "$image" --input-type=module --eval '
      import fs from "node:fs";
      import { connect as clientConnect } from "@husklet/client";
      import clientManifest from "@husklet/client/package.json" with { type: "json" };
      import { connect, render } from "@husklet/react";
      import manifest from "@husklet/react/package.json" with { type: "json" };
      if (process.getuid?.() === 0) throw new Error("base runs as root");
      if (process.versions.node !== "22.23.2") throw new Error(`Node ${process.versions.node}`);
      if (process.env.HUSKLET_EXTENSION_SOCKET !== "/run/husklet/extension.sock") throw new Error("socket env missing");
      if (manifest.version !== process.env.EXPECTED_VERSION) throw new Error(`SDK ${manifest.version}`);
      if (clientManifest.version !== process.env.EXPECTED_VERSION) throw new Error(`client ${clientManifest.version}`);
      if (connect !== clientConnect) throw new Error("React does not expose the installed client runtime");
      if (typeof connect !== "function" || typeof render !== "function") throw new Error("SDK runtime unavailable");
      const starter = "/app/node_modules/@husklet/react/examples/starter";
      for (const file of ["Dockerfile", "extension.toml", "main.js", "package.json"]) {
        if (!fs.statSync(`${starter}/${file}`).isFile()) throw new Error(`starter omits ${file}`);
      }
      const starterManifest = fs.readFileSync(`${starter}/extension.toml`, "utf8");
      if (!starterManifest.includes(`version = "${process.env.EXPECTED_VERSION}"`)) {
        throw new Error("starter manifest version does not match SDK/base image");
      }
    '
  [[ "$(docker run --rm --platform "$platform" --entrypoint npm "$image" --version)" == 10.9.8 ]] \
    || fail "$image npm executable does not match its pinned label"
else
  [[ "$(inspect '{{index .Config.Labels "husklet.extension.manifest"}}')" == /etc/husklet/extension.toml ]] \
    || fail "$image does not point at its packaged manifest"
  [[ "$(inspect '{{json .Config.Cmd}}')" == '["node","/app/src/main.js"]' ]] \
    || fail "$image does not launch its packaged entrypoint"
  docker run --rm --platform "$platform" --entrypoint node \
    -e EXPECTED_VERSION="$version" -e EXPECTED_EXTENSION="$kind" "$image" --input-type=module --eval '
      import fs from "node:fs";
      import { connect } from "@husklet/client";
      import client from "@husklet/client/package.json" with { type: "json" };
      import sdk from "@husklet/react/package.json" with { type: "json" };
      const manifest = fs.readFileSync("/etc/husklet/extension.toml", "utf8");
      if (process.getuid?.() === 0) throw new Error("extension runs as root");
      if (process.versions.node !== "22.23.2") throw new Error(`Node ${process.versions.node}`);
      if (process.env.HUSKLET_EXTENSION_SOCKET !== "/run/husklet/extension.sock") throw new Error("socket env missing");
      if (sdk.version !== process.env.EXPECTED_VERSION) throw new Error(`SDK ${sdk.version}`);
      if (client.version !== process.env.EXPECTED_VERSION || typeof connect !== "function") throw new Error("client runtime unavailable");
      if (!manifest.includes(`name = "${process.env.EXPECTED_EXTENSION}"`)) throw new Error("wrong manifest name");
      if (!manifest.includes(`version = "${process.env.EXPECTED_VERSION}"`)) throw new Error("wrong manifest version");
      if (!manifest.includes("protocol = 1")) throw new Error("wrong manifest protocol");
      if (!fs.statSync("/app/src/main.js").isFile()) throw new Error("entrypoint missing");
    '
fi

echo "$image passed $platform packaged image smoke"
