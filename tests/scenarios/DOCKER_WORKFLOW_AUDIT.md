# Docker workflow ownership audit

This audit records why the former `docker` workflow was removed and why one
small `docker-full` remainder still exists. The future provider/cache pipeline
in `tests/PIPELINE.md` is documentation only and was not implemented here.

## Closed behavior

The removed orchestration repeated public contracts already owned by packages:

| Former workflow behavior | Owning public-contract evidence |
|---|---|
| ping, version, info, disk usage | `hl-client/tests/daemon/system.rs::system_contract_is_platform_derived_and_unsupported_routes_are_explicit` and `hl-daemon/tests/system_disk.rs::wire_client` |
| image load, list, inspect, history, tag, save, remove, reload | `hl-client/tests/daemon/image.rs::image_archive_round_trip_uses_shared_wire_contracts` and `image_archive_tag_save_remove_and_prune_share_wire_contracts`; `hl-daemon/tests/api/image_archive.rs` |
| foreground exit, stdout/stderr logs | `hl-daemon/tests/api/headless_runtime.rs` and `hl-daemon/tests/api/daemon_runtime.rs` |
| attach stdin/stdout/stderr and exec exit/output | `hl-daemon/tests/api/daemon_runtime.rs` |
| update and restart policy | `hl-client/tests/daemon/update.rs::container_update_persists_effective_settings_and_rejects_unknown_fields` |
| create/start/die/destroy event replay | `hl-client/tests/daemon/event.rs::typed_event_stream_replays_create_and_destroy_from_real_handlers` |
| container changes and commit | `hl-client/tests/daemon/metadata.rs::container_changes_compare_owned_rootfs_with_immutable_image_baseline` and `hl-client/tests/daemon/filesystem.rs::image_list_shared_size_accounts_executed_child_layers` |
| container export | `hl-client/tests/daemon/observability.rs::container_archive_round_trip_streams_through_typed_client` and `hl-daemon/tests/container_export.rs::wire_contract` |
| volume create/list/remove | `hl-client/tests/daemon/volume.rs::volume_crud_is_shared_with_headless_ownership_and_protects_references` |
| network create/list/remove | `hl-client/tests/daemon/network.rs::network_client_and_server_share_headless_topology` and `forced_network_removal_uses_the_docker_delete_contract` |
| plugins, authentication, search | `hl-client/tests/daemon/metadata.rs::compatibility_metadata_surfaces_are_typed_and_truthful` |
| resource prune verbs | `hl-client/tests/system.rs::system_prune_reclaims_unused_resources_and_respects_volume_selection`, plus daemon image/network prune tests |

Those tests are closer to the owning API boundary, independently removable,
and report their exact failed contract instead of one aggregate workflow label.
The detached `docker_container.rs` orchestration and the redundant `docker`
workflow name therefore had no remaining ownership role.

## Retained gap

No package test exercises the successful Docker-compatible root-filesystem
import path (`POST /images/create?fromSrc=-`) through the typed client and then
discovers the requested repository tag. `docker-full` is reduced to only that
contract and its necessary image/container/export setup. Once an owning
`hl-client`/`hl-daemon` public-contract test covers this route, the remaining
workflow and its inventory name can be deleted together.
