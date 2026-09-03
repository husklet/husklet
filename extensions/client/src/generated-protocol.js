// Generated from Rust hl-extension protocol/v1.json. Do not edit.
export const PROTOCOL_SPECIFICATION_VERSION = 1;
export const PROTOCOL_VERSION = 1;
export const PROTOCOL_BOUNDS = Object.freeze({
  "extension_job_bytes": 128,
  "extension_reference_bytes": 512,
  "pane_input_bytes": 65536,
  "pane_inventory_items": 512,
  "pane_text_bytes": 524288,
  "semantic_action_value_bytes": 4096,
  "semantic_depth": 32,
  "semantic_nodes": 256,
  "semantic_text_bytes": 256,
  "terminal_command_argument_bytes": 4096,
  "terminal_command_bytes": 32768
});
export const PROTOCOL_CAPABILITIES = Object.freeze([
  {
    "executes": false,
    "mutates": false,
    "wire": "workspace-read"
  },
  {
    "executes": true,
    "mutates": true,
    "wire": "workspace-control"
  },
  {
    "executes": false,
    "mutates": false,
    "wire": "workspace-events"
  },
  {
    "executes": false,
    "mutates": false,
    "wire": "container-read"
  },
  {
    "executes": true,
    "mutates": true,
    "wire": "container-control"
  },
  {
    "executes": true,
    "mutates": true,
    "wire": "container-attach"
  },
  {
    "executes": false,
    "mutates": false,
    "wire": "image-read"
  },
  {
    "executes": false,
    "mutates": true,
    "wire": "image-write"
  },
  {
    "executes": false,
    "mutates": false,
    "wire": "volume-read"
  },
  {
    "executes": false,
    "mutates": true,
    "wire": "volume-write"
  },
  {
    "executes": false,
    "mutates": false,
    "wire": "network-read"
  },
  {
    "executes": false,
    "mutates": true,
    "wire": "network-write"
  },
  {
    "executes": false,
    "mutates": false,
    "wire": "terminal-read"
  },
  {
    "executes": true,
    "mutates": true,
    "wire": "terminal-control"
  },
  {
    "executes": false,
    "mutates": false,
    "wire": "terminal-output"
  },
  {
    "executes": false,
    "mutates": false,
    "wire": "pane-observe"
  },
  {
    "executes": false,
    "mutates": false,
    "wire": "pane-semantic-read"
  },
  {
    "executes": false,
    "mutates": true,
    "wire": "pane-semantic-control"
  },
  {
    "executes": false,
    "mutates": false,
    "wire": "extension-read"
  },
  {
    "executes": false,
    "mutates": true,
    "wire": "extension-control"
  },
  {
    "executes": false,
    "mutates": true,
    "wire": "extension-install"
  },
  {
    "executes": false,
    "mutates": false,
    "wire": "filesystem-read"
  },
  {
    "executes": false,
    "mutates": true,
    "wire": "filesystem-write"
  },
  {
    "executes": false,
    "mutates": false,
    "wire": "interface"
  }
]);
export const PROTOCOL_TOPICS = Object.freeze([
  {
    "capability": "container-read",
    "snapshot": "containers",
    "wire": "containers"
  },
  {
    "capability": "container-read",
    "snapshot": "container_inventory",
    "wire": "container-inventory"
  },
  {
    "capability": "container-read",
    "snapshot": "executions",
    "wire": "executions"
  },
  {
    "capability": "image-read",
    "snapshot": "images",
    "wire": "images"
  },
  {
    "capability": "image-write",
    "snapshot": "image_pulls",
    "wire": "image-pulls"
  },
  {
    "capability": "volume-read",
    "snapshot": "volumes",
    "wire": "volumes"
  },
  {
    "capability": "network-read",
    "snapshot": "networks",
    "wire": "networks"
  },
  {
    "capability": "terminal-read",
    "snapshot": "terminal",
    "wire": "terminal"
  },
  {
    "capability": "pane-observe",
    "snapshot": "pane_changes",
    "wire": "pane-changes"
  },
  {
    "capability": "extension-read",
    "snapshot": "extensions",
    "wire": "extensions"
  },
  {
    "capability": "extension-install",
    "snapshot": "extension_acquisitions",
    "wire": "extension-acquisitions"
  },
  {
    "capability": "workspace-read",
    "snapshot": "workspace_lifecycle",
    "wire": "workspace-lifecycle"
  },
  {
    "capability": "workspace-events",
    "snapshot": "workspace_events",
    "wire": "workspace-events"
  }
]);
export const PROTOCOL_REPLIES = Object.freeze({
  "workspace_info": "workspace",
  "workspace_list": "workspaces",
  "workspace_inspect": "workspace_configuration",
  "workspace_create": "workspace_configuration",
  "workspace_adopt": "workspace_configuration",
  "workspace_update": "workspace_configuration",
  "workspace_delete": "done",
  "workspace_start": "done",
  "workspace_stop": "done",
  "workspace_restart": "done",
  "extension_list": "extensions",
  "extension_inspect": "extension",
  "extension_enable": "done",
  "extension_disable": "done",
  "extension_retry": "done",
  "extension_remove": "done",
  "extension_acquisition_start": "extension_acquisition_job",
  "extension_acquisition_status": "extension_acquisition",
  "extension_acquisition_cancel": "done",
  "extension_install": "extension",
  "extension_update": "extension",
  "container_list": "containers",
  "container_inspect": "container",
  "container_processes": "processes",
  "container_logs": "logs",
  "execution_inspect": "execution",
  "execution_list": "executions",
  "execution_logs": "logs",
  "execution_wait": "execution",
  "execution_kill": "done",
  "execution_remove": "done",
  "container_create": "identity",
  "container_start": "done",
  "container_stop": "done",
  "container_remove": "done",
  "container_pause": "done",
  "container_unpause": "done",
  "container_restart": "done",
  "container_rename": "done",
  "container_kill": "done",
  "container_exec": "identity",
  "container_attach_terminal": "identity",
  "image_list": "images",
  "image_pull": "image",
  "image_pull_start": "image_pull_job",
  "image_pull_status": "image_pull",
  "image_pull_cancel": "done",
  "image_inspect": "image_details",
  "image_remove": "done",
  "image_prune": "image_prune",
  "volume_list": "volumes",
  "volume_inspect": "volume",
  "volume_create": "volume",
  "volume_remove": "done",
  "network_list": "networks",
  "network_inspect": "network",
  "network_create": "identity",
  "network_remove": "done",
  "network_connect": "done",
  "network_disconnect": "done",
  "terminal_tabs": "tabs",
  "terminal_topology": "topology",
  "pane_list": "panes",
  "terminal_open_tab": "identity",
  "terminal_split": "identity",
  "terminal_split_observed": "identity",
  "terminal_spawn": "done",
  "terminal_spawn_observed": "done",
  "terminal_read_pane": "text",
  "pane_semantic_read": "semantics",
  "pane_semantic_action": "done",
  "terminal_write_pane": "done",
  "terminal_resize_grid": "done",
  "terminal_resize_grid_observed": "done",
  "terminal_close_pane": "done",
  "terminal_close_pane_observed": "done",
  "terminal_focus_pane": "done",
  "terminal_focus_pane_observed": "done",
  "terminal_retitle_pane": "done",
  "terminal_retitle_pane_observed": "done",
  "terminal_ratio": "done",
  "terminal_ratio_observed": "done",
  "terminal_switch_occupant": "done",
  "terminal_switch_occupant_observed": "done",
  "filesystem_list": "entries",
  "filesystem_read": "contents",
  "filesystem_read_range": "file_range",
  "filesystem_stat": "entry",
  "filesystem_write": "done",
  "filesystem_create_observed": "identity",
  "filesystem_mkdir": "done",
  "filesystem_rename": "done",
  "filesystem_rename_observed": "identity",
  "filesystem_remove": "done",
  "filesystem_remove_observed": "done",
  "interface_open_tab": "identity",
  "interface_split": "identity",
  "interface_withdraw": "done",
  "interface_render": "done",
  "interface_render_at": "done",
  "source_resize": "done",
  "source_resize_at": "done",
  "event_subscribe": "done",
  "event_unsubscribe": "done"
});
export const PROTOCOL_REQUEST_CAPABILITIES = Object.freeze({
  "workspace_info": "workspace-read",
  "workspace_list": "workspace-read",
  "workspace_inspect": "workspace-read",
  "workspace_create": "workspace-control",
  "workspace_adopt": "workspace-control",
  "workspace_update": "workspace-control",
  "workspace_delete": "workspace-control",
  "workspace_start": "workspace-control",
  "workspace_stop": "workspace-control",
  "workspace_restart": "workspace-control",
  "extension_list": "extension-read",
  "extension_inspect": "extension-read",
  "extension_enable": "extension-control",
  "extension_disable": "extension-control",
  "extension_retry": "extension-control",
  "extension_remove": "extension-control",
  "extension_acquisition_start": "extension-install",
  "extension_acquisition_status": "extension-install",
  "extension_acquisition_cancel": "extension-install",
  "extension_install": "extension-install",
  "extension_update": "extension-install",
  "container_list": "container-read",
  "container_inspect": "container-read",
  "container_processes": "container-read",
  "container_logs": "container-read",
  "execution_inspect": "container-read",
  "execution_list": "container-read",
  "execution_logs": "container-read",
  "execution_wait": "container-read",
  "execution_kill": "container-control",
  "execution_remove": "container-control",
  "container_create": "container-control",
  "container_start": "container-control",
  "container_stop": "container-control",
  "container_remove": "container-control",
  "container_pause": "container-control",
  "container_unpause": "container-control",
  "container_restart": "container-control",
  "container_rename": "container-control",
  "container_kill": "container-control",
  "container_exec": "container-control",
  "container_attach_terminal": "container-attach",
  "image_list": "image-read",
  "image_pull": "image-write",
  "image_pull_start": "image-write",
  "image_pull_status": "image-write",
  "image_pull_cancel": "image-write",
  "image_inspect": "image-read",
  "image_remove": "image-write",
  "image_prune": "image-write",
  "volume_list": "volume-read",
  "volume_inspect": "volume-read",
  "volume_create": "volume-write",
  "volume_remove": "volume-write",
  "network_list": "network-read",
  "network_inspect": "network-read",
  "network_create": "network-write",
  "network_remove": "network-write",
  "network_connect": "network-write",
  "network_disconnect": "network-write",
  "terminal_tabs": "terminal-read",
  "terminal_topology": "terminal-read",
  "pane_list": "pane-observe",
  "terminal_open_tab": "terminal-control",
  "terminal_split": "terminal-control",
  "terminal_split_observed": "terminal-control",
  "terminal_spawn": "terminal-control",
  "terminal_spawn_observed": "terminal-control",
  "terminal_read_pane": "terminal-output",
  "pane_semantic_read": "pane-semantic-read",
  "pane_semantic_action": "pane-semantic-control",
  "terminal_write_pane": "terminal-control",
  "terminal_resize_grid": "terminal-control",
  "terminal_resize_grid_observed": "terminal-control",
  "terminal_close_pane": "terminal-control",
  "terminal_close_pane_observed": "terminal-control",
  "terminal_focus_pane": "terminal-control",
  "terminal_focus_pane_observed": "terminal-control",
  "terminal_retitle_pane": "terminal-control",
  "terminal_retitle_pane_observed": "terminal-control",
  "terminal_ratio": "terminal-control",
  "terminal_ratio_observed": "terminal-control",
  "terminal_switch_occupant": "terminal-control",
  "terminal_switch_occupant_observed": "terminal-control",
  "filesystem_list": "filesystem-read",
  "filesystem_read": "filesystem-read",
  "filesystem_read_range": "filesystem-read",
  "filesystem_stat": "filesystem-read",
  "filesystem_write": "filesystem-write",
  "filesystem_create_observed": "filesystem-write",
  "filesystem_mkdir": "filesystem-write",
  "filesystem_rename": "filesystem-write",
  "filesystem_rename_observed": "filesystem-write",
  "filesystem_remove": "filesystem-write",
  "filesystem_remove_observed": "filesystem-write",
  "interface_open_tab": "interface",
  "interface_split": "interface",
  "interface_withdraw": "interface",
  "interface_render": "interface",
  "interface_render_at": "interface",
  "source_resize": "interface",
  "source_resize_at": "interface",
  "event_subscribe": null,
  "event_unsubscribe": null
});
const definitions = {
  "Align": {
    "kind": "enum",
    "serde": {},
    "variants": [
      {
        "name": "Start",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Center",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "End",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Stretch",
        "payload": {
          "kind": "unit"
        }
      }
    ]
  },
  "Bounds": {
    "fields": [
      {
        "name": "minimum",
        "optional": true,
        "schema": {
          "kind": "optional",
          "of": {
            "kind": "ref",
            "name": "Length"
          }
        }
      },
      {
        "name": "maximum",
        "optional": true,
        "schema": {
          "kind": "optional",
          "of": {
            "kind": "ref",
            "name": "Length"
          }
        }
      }
    ],
    "kind": "struct",
    "serde": {}
  },
  "Capability": {
    "kind": "enum",
    "serde": {
      "rename_all": "kebab-case"
    },
    "variants": [
      {
        "name": "workspace-read",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "workspace-control",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "workspace-events",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "container-read",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "container-control",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "container-attach",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "image-read",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "image-write",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "volume-read",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "volume-write",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "network-read",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "network-write",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "terminal-read",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "terminal-control",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "terminal-output",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "pane-observe",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "pane-semantic-read",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "pane-semantic-control",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "extension-read",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "extension-control",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "extension-install",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "filesystem-read",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "filesystem-write",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "interface",
        "payload": {
          "kind": "unit"
        }
      }
    ]
  },
  "Cell": {
    "kind": "enum",
    "serde": {},
    "variants": [
      {
        "name": "Text",
        "payload": {
          "kind": "newtype",
          "of": {
            "kind": "string"
          }
        }
      },
      {
        "name": "Number",
        "payload": {
          "kind": "newtype",
          "of": {
            "bits": 64,
            "kind": "float"
          }
        }
      },
      {
        "name": "Bytes",
        "payload": {
          "kind": "newtype",
          "of": {
            "bits": 64,
            "kind": "integer",
            "maximum": 9007199254740991,
            "minimum": 0,
            "signed": false
          }
        }
      },
      {
        "name": "Badge",
        "payload": {
          "fields": [
            {
              "name": "label",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            },
            {
              "name": "tone",
              "optional": false,
              "schema": {
                "kind": "ref",
                "name": "Tone"
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "Stamp",
        "payload": {
          "kind": "newtype",
          "of": {
            "bits": 64,
            "kind": "integer",
            "maximum": 9007199254740991,
            "minimum": -9007199254740991,
            "signed": true
          }
        }
      },
      {
        "name": "Empty",
        "payload": {
          "kind": "unit"
        }
      }
    ]
  },
  "Choice": {
    "fields": [
      {
        "name": "value",
        "optional": false,
        "schema": {
          "kind": "string"
        }
      },
      {
        "name": "label",
        "optional": false,
        "schema": {
          "kind": "string"
        }
      }
    ],
    "kind": "struct",
    "serde": {}
  },
  "Column": {
    "fields": [
      {
        "name": "key",
        "optional": false,
        "schema": {
          "kind": "string"
        }
      },
      {
        "name": "title",
        "optional": false,
        "schema": {
          "kind": "string"
        }
      },
      {
        "name": "width",
        "optional": false,
        "schema": {
          "kind": "ref",
          "name": "Length"
        }
      },
      {
        "name": "align",
        "optional": false,
        "schema": {
          "kind": "ref",
          "name": "Align"
        }
      },
      {
        "name": "sortable",
        "optional": false,
        "schema": {
          "kind": "boolean"
        }
      },
      {
        "name": "editable",
        "optional": false,
        "schema": {
          "kind": "boolean"
        }
      }
    ],
    "kind": "struct",
    "serde": {}
  },
  "ContainerCreateSpec": {
    "fields": [
      {
        "name": "image",
        "optional": false,
        "schema": {
          "kind": "string"
        }
      },
      {
        "name": "name",
        "optional": false,
        "schema": {
          "kind": "string"
        }
      },
      {
        "name": "hostname",
        "optional": true,
        "schema": {
          "kind": "optional",
          "of": {
            "kind": "string"
          }
        }
      },
      {
        "name": "entrypoint",
        "optional": true,
        "schema": {
          "kind": "optional",
          "of": {
            "kind": "array",
            "of": {
              "kind": "string"
            }
          }
        }
      },
      {
        "name": "command",
        "optional": false,
        "schema": {
          "kind": "array",
          "of": {
            "kind": "string"
          }
        }
      },
      {
        "name": "environment",
        "optional": false,
        "schema": {
          "kind": "array",
          "of": {
            "items": [
              {
                "kind": "string"
              },
              {
                "kind": "string"
              }
            ],
            "kind": "tuple"
          }
        }
      },
      {
        "name": "working_directory",
        "optional": true,
        "schema": {
          "kind": "optional",
          "of": {
            "kind": "string"
          }
        }
      },
      {
        "name": "user",
        "optional": true,
        "schema": {
          "kind": "optional",
          "of": {
            "kind": "string"
          }
        }
      },
      {
        "name": "labels",
        "optional": false,
        "schema": {
          "kind": "array",
          "of": {
            "items": [
              {
                "kind": "string"
              },
              {
                "kind": "string"
              }
            ],
            "kind": "tuple"
          }
        }
      },
      {
        "name": "mounts",
        "optional": false,
        "schema": {
          "kind": "array",
          "of": {
            "kind": "ref",
            "name": "ContainerVolumeMount"
          }
        }
      },
      {
        "name": "network",
        "optional": true,
        "schema": {
          "kind": "optional",
          "of": {
            "kind": "string"
          }
        }
      },
      {
        "name": "ports",
        "optional": false,
        "schema": {
          "kind": "array",
          "of": {
            "kind": "ref",
            "name": "ContainerPort"
          }
        }
      },
      {
        "name": "memory_mb",
        "optional": true,
        "schema": {
          "kind": "optional",
          "of": {
            "bits": 32,
            "kind": "integer",
            "maximum": 4294967295,
            "minimum": 0,
            "signed": false
          }
        }
      },
      {
        "name": "cpus",
        "optional": true,
        "schema": {
          "kind": "optional",
          "of": {
            "bits": 16,
            "kind": "integer",
            "maximum": 65535,
            "minimum": 0,
            "signed": false
          }
        }
      },
      {
        "name": "pids_limit",
        "optional": true,
        "schema": {
          "kind": "optional",
          "of": {
            "bits": 32,
            "kind": "integer",
            "maximum": 4294967295,
            "minimum": 0,
            "signed": false
          }
        }
      }
    ],
    "kind": "struct",
    "serde": {}
  },
  "ContainerInventory": {
    "fields": [
      {
        "name": "containers",
        "optional": false,
        "schema": {
          "kind": "array",
          "of": {
            "kind": "ref",
            "name": "ContainerSummary"
          }
        }
      },
      {
        "name": "complete",
        "optional": false,
        "schema": {
          "kind": "boolean"
        }
      }
    ],
    "kind": "struct",
    "serde": {}
  },
  "ContainerOutput": {
    "fields": [
      {
        "name": "stdout",
        "optional": false,
        "schema": {
          "kind": "array",
          "of": {
            "bits": 8,
            "kind": "integer",
            "maximum": 255,
            "minimum": 0,
            "signed": false
          }
        }
      },
      {
        "name": "stderr",
        "optional": false,
        "schema": {
          "kind": "array",
          "of": {
            "bits": 8,
            "kind": "integer",
            "maximum": 255,
            "minimum": 0,
            "signed": false
          }
        }
      },
      {
        "name": "truncated",
        "optional": false,
        "schema": {
          "kind": "boolean"
        }
      },
      {
        "name": "stdout_truncated",
        "optional": true,
        "schema": {
          "kind": "boolean"
        }
      },
      {
        "name": "stderr_truncated",
        "optional": true,
        "schema": {
          "kind": "boolean"
        }
      },
      {
        "name": "eof",
        "optional": true,
        "schema": {
          "kind": "boolean"
        }
      }
    ],
    "kind": "struct",
    "serde": {}
  },
  "ContainerPort": {
    "fields": [
      {
        "name": "container",
        "optional": false,
        "schema": {
          "bits": 16,
          "kind": "integer",
          "maximum": 65535,
          "minimum": 0,
          "signed": false
        }
      },
      {
        "name": "host",
        "optional": true,
        "schema": {
          "kind": "optional",
          "of": {
            "bits": 16,
            "kind": "integer",
            "maximum": 65535,
            "minimum": 0,
            "signed": false
          }
        }
      },
      {
        "name": "protocol",
        "optional": false,
        "schema": {
          "kind": "string"
        }
      }
    ],
    "kind": "struct",
    "serde": {}
  },
  "ContainerSummary": {
    "fields": [
      {
        "name": "id",
        "optional": false,
        "schema": {
          "kind": "string"
        }
      },
      {
        "name": "name",
        "optional": false,
        "schema": {
          "kind": "string"
        }
      },
      {
        "name": "image",
        "optional": false,
        "schema": {
          "kind": "string"
        }
      },
      {
        "name": "state",
        "optional": false,
        "schema": {
          "kind": "string"
        }
      },
      {
        "name": "created",
        "optional": false,
        "schema": {
          "bits": 64,
          "kind": "integer",
          "maximum": 9007199254740991,
          "minimum": -9007199254740991,
          "signed": true
        }
      }
    ],
    "kind": "struct",
    "serde": {}
  },
  "ContainerVolumeMount": {
    "fields": [
      {
        "name": "volume",
        "optional": false,
        "schema": {
          "kind": "string"
        }
      },
      {
        "name": "target",
        "optional": false,
        "schema": {
          "kind": "string"
        }
      },
      {
        "name": "read_only",
        "optional": false,
        "schema": {
          "kind": "boolean"
        }
      }
    ],
    "kind": "struct",
    "serde": {}
  },
  "Division": {
    "kind": "enum",
    "serde": {
      "rename_all": "kebab-case"
    },
    "variants": [
      {
        "name": "beside",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "below",
        "payload": {
          "kind": "unit"
        }
      }
    ]
  },
  "Edges": {
    "fields": [
      {
        "name": "top",
        "optional": false,
        "schema": {
          "kind": "ref",
          "name": "Length"
        }
      },
      {
        "name": "end",
        "optional": false,
        "schema": {
          "kind": "ref",
          "name": "Length"
        }
      },
      {
        "name": "bottom",
        "optional": false,
        "schema": {
          "kind": "ref",
          "name": "Length"
        }
      },
      {
        "name": "start",
        "optional": false,
        "schema": {
          "kind": "ref",
          "name": "Length"
        }
      }
    ],
    "kind": "struct",
    "serde": {}
  },
  "Entry": {
    "fields": [
      {
        "name": "path",
        "optional": false,
        "schema": {
          "kind": "ref",
          "name": "RelativePath"
        }
      },
      {
        "name": "directory",
        "optional": false,
        "schema": {
          "kind": "boolean"
        }
      },
      {
        "name": "size",
        "optional": false,
        "schema": {
          "bits": 64,
          "kind": "integer",
          "maximum": 9007199254740991,
          "minimum": 0,
          "signed": false
        }
      },
      {
        "name": "identity",
        "optional": true,
        "schema": {
          "kind": "optional",
          "of": {
            "kind": "string"
          }
        }
      }
    ],
    "kind": "struct",
    "serde": {}
  },
  "EventId": {
    "kind": "newtype",
    "of": {
      "kind": "string"
    },
    "serde": {}
  },
  "ExecutionList": {
    "fields": [
      {
        "name": "executions",
        "optional": false,
        "schema": {
          "kind": "array",
          "of": {
            "kind": "ref",
            "name": "ExecutionSummary"
          }
        }
      },
      {
        "name": "truncated",
        "optional": false,
        "schema": {
          "kind": "boolean"
        }
      }
    ],
    "kind": "struct",
    "serde": {}
  },
  "ExecutionSummary": {
    "fields": [
      {
        "name": "id",
        "optional": false,
        "schema": {
          "kind": "string"
        }
      },
      {
        "name": "container_id",
        "optional": false,
        "schema": {
          "kind": "string"
        }
      },
      {
        "name": "running",
        "optional": false,
        "schema": {
          "kind": "boolean"
        }
      },
      {
        "name": "exit_code",
        "optional": false,
        "schema": {
          "bits": 64,
          "kind": "integer",
          "maximum": 9007199254740991,
          "minimum": -9007199254740991,
          "signed": true
        }
      },
      {
        "name": "pid",
        "optional": false,
        "schema": {
          "bits": 64,
          "kind": "integer",
          "maximum": 9007199254740991,
          "minimum": -9007199254740991,
          "signed": true
        }
      },
      {
        "name": "command",
        "optional": false,
        "schema": {
          "kind": "array",
          "of": {
            "kind": "string"
          }
        }
      },
      {
        "name": "user",
        "optional": false,
        "schema": {
          "kind": "string"
        }
      }
    ],
    "kind": "struct",
    "serde": {}
  },
  "ExtensionAcquisitionChange": {
    "fields": [
      {
        "name": "job",
        "optional": false,
        "schema": {
          "kind": "string"
        }
      },
      {
        "name": "revision",
        "optional": false,
        "schema": {
          "bits": 64,
          "kind": "integer",
          "maximum": 9007199254740991,
          "minimum": 0,
          "signed": false
        }
      },
      {
        "name": "state",
        "optional": false,
        "schema": {
          "kind": "string"
        }
      },
      {
        "name": "coalesced",
        "optional": false,
        "schema": {
          "bits": 64,
          "kind": "integer",
          "maximum": 9007199254740991,
          "minimum": 0,
          "signed": false
        }
      }
    ],
    "kind": "struct",
    "serde": {}
  },
  "ExtensionAcquisitionJob": {
    "fields": [
      {
        "name": "job",
        "optional": false,
        "schema": {
          "kind": "string"
        }
      }
    ],
    "kind": "struct",
    "serde": {}
  },
  "ExtensionAcquisitionProgress": {
    "fields": [
      {
        "name": "status",
        "optional": false,
        "schema": {
          "kind": "string"
        }
      },
      {
        "name": "id",
        "optional": true,
        "schema": {
          "kind": "optional",
          "of": {
            "kind": "string"
          }
        }
      },
      {
        "name": "current",
        "optional": true,
        "schema": {
          "kind": "optional",
          "of": {
            "bits": 64,
            "kind": "integer",
            "maximum": 9007199254740991,
            "minimum": 0,
            "signed": false
          }
        }
      },
      {
        "name": "total",
        "optional": true,
        "schema": {
          "kind": "optional",
          "of": {
            "bits": 64,
            "kind": "integer",
            "maximum": 9007199254740991,
            "minimum": 0,
            "signed": false
          }
        }
      }
    ],
    "kind": "struct",
    "serde": {}
  },
  "ExtensionAcquisitionStatus": {
    "fields": [
      {
        "name": "job",
        "optional": false,
        "schema": {
          "kind": "string"
        }
      },
      {
        "name": "reference",
        "optional": false,
        "schema": {
          "kind": "string"
        }
      },
      {
        "name": "revision",
        "optional": false,
        "schema": {
          "bits": 64,
          "kind": "integer",
          "maximum": 9007199254740991,
          "minimum": 0,
          "signed": false
        }
      },
      {
        "name": "state",
        "optional": false,
        "schema": {
          "kind": "string"
        }
      },
      {
        "name": "progress",
        "optional": true,
        "schema": {
          "kind": "optional",
          "of": {
            "kind": "ref",
            "name": "ExtensionAcquisitionProgress"
          }
        }
      },
      {
        "name": "candidate",
        "optional": true,
        "schema": {
          "kind": "optional",
          "of": {
            "kind": "ref",
            "name": "ExtensionCandidate"
          }
        }
      },
      {
        "name": "error",
        "optional": true,
        "schema": {
          "kind": "optional",
          "of": {
            "kind": "string"
          }
        }
      }
    ],
    "kind": "struct",
    "serde": {}
  },
  "ExtensionCandidate": {
    "fields": [
      {
        "name": "name",
        "optional": false,
        "schema": {
          "kind": "ref",
          "name": "ExtensionName"
        }
      },
      {
        "name": "version",
        "optional": false,
        "schema": {
          "kind": "string"
        }
      },
      {
        "name": "image_digest",
        "optional": false,
        "schema": {
          "kind": "string"
        }
      },
      {
        "name": "requested",
        "optional": false,
        "schema": {
          "kind": "ref",
          "name": "Grant"
        }
      },
      {
        "name": "installed_image_digest",
        "optional": true,
        "schema": {
          "kind": "optional",
          "of": {
            "kind": "string"
          }
        }
      }
    ],
    "kind": "struct",
    "serde": {}
  },
  "ExtensionName": {
    "kind": "ref",
    "name": "PeerName"
  },
  "ExtensionSummary": {
    "fields": [
      {
        "name": "name",
        "optional": false,
        "schema": {
          "kind": "string"
        }
      },
      {
        "name": "image_digest",
        "optional": false,
        "schema": {
          "kind": "string"
        }
      },
      {
        "name": "status",
        "optional": false,
        "schema": {
          "kind": "string"
        }
      },
      {
        "name": "version",
        "optional": true,
        "schema": {
          "kind": "string"
        }
      },
      {
        "name": "enabled",
        "optional": true,
        "schema": {
          "kind": "boolean"
        }
      },
      {
        "name": "pane_providers",
        "optional": true,
        "schema": {
          "kind": "array",
          "of": {
            "kind": "ref",
            "name": "PaneProvider"
          }
        }
      }
    ],
    "kind": "struct",
    "serde": {}
  },
  "FileRange": {
    "fields": [
      {
        "name": "path",
        "optional": false,
        "schema": {
          "kind": "ref",
          "name": "RelativePath"
        }
      },
      {
        "name": "identity",
        "optional": false,
        "schema": {
          "kind": "string"
        }
      },
      {
        "name": "offset",
        "optional": false,
        "schema": {
          "bits": 64,
          "kind": "integer",
          "maximum": 9007199254740991,
          "minimum": 0,
          "signed": false
        }
      },
      {
        "name": "total",
        "optional": false,
        "schema": {
          "bits": 64,
          "kind": "integer",
          "maximum": 9007199254740991,
          "minimum": 0,
          "signed": false
        }
      },
      {
        "name": "contents",
        "optional": false,
        "schema": {
          "kind": "array",
          "of": {
            "bits": 8,
            "kind": "integer",
            "maximum": 255,
            "minimum": 0,
            "signed": false
          }
        }
      },
      {
        "name": "eof",
        "optional": false,
        "schema": {
          "kind": "boolean"
        }
      },
      {
        "name": "truncated",
        "optional": false,
        "schema": {
          "kind": "boolean"
        }
      }
    ],
    "kind": "struct",
    "serde": {}
  },
  "Frame": {
    "fields": [
      {
        "name": "sequence",
        "optional": false,
        "schema": {
          "bits": 64,
          "kind": "integer",
          "maximum": 9007199254740991,
          "minimum": 0,
          "signed": false
        }
      },
      {
        "name": "patches",
        "optional": false,
        "schema": {
          "kind": "array",
          "of": {
            "kind": "ref",
            "name": "Patch"
          }
        }
      }
    ],
    "kind": "struct",
    "serde": {}
  },
  "Grant": {
    "kind": "array",
    "of": {
      "kind": "ref",
      "name": "Capability"
    },
    "unique": true
  },
  "GridSize": {
    "fields": [
      {
        "name": "columns",
        "optional": false,
        "schema": {
          "bits": 16,
          "kind": "integer",
          "maximum": 65535,
          "minimum": 0,
          "signed": false
        }
      },
      {
        "name": "rows",
        "optional": false,
        "schema": {
          "bits": 16,
          "kind": "integer",
          "maximum": 65535,
          "minimum": 0,
          "signed": false
        }
      }
    ],
    "kind": "struct",
    "serde": {}
  },
  "Handler": {
    "fields": [
      {
        "name": "trigger",
        "optional": false,
        "schema": {
          "kind": "ref",
          "name": "Trigger"
        }
      },
      {
        "name": "id",
        "optional": false,
        "schema": {
          "kind": "ref",
          "name": "EventId"
        }
      }
    ],
    "kind": "struct",
    "serde": {}
  },
  "ImageDetails": {
    "fields": [
      {
        "name": "id",
        "optional": false,
        "schema": {
          "kind": "string"
        }
      },
      {
        "name": "references",
        "optional": false,
        "schema": {
          "kind": "array",
          "of": {
            "kind": "string"
          }
        }
      },
      {
        "name": "created",
        "optional": false,
        "schema": {
          "kind": "string"
        }
      },
      {
        "name": "size",
        "optional": false,
        "schema": {
          "bits": 64,
          "kind": "integer",
          "maximum": 9007199254740991,
          "minimum": 0,
          "signed": false
        }
      },
      {
        "name": "os",
        "optional": false,
        "schema": {
          "kind": "string"
        }
      },
      {
        "name": "architecture",
        "optional": false,
        "schema": {
          "kind": "string"
        }
      },
      {
        "name": "entrypoint",
        "optional": false,
        "schema": {
          "kind": "array",
          "of": {
            "kind": "string"
          }
        }
      },
      {
        "name": "command",
        "optional": false,
        "schema": {
          "kind": "array",
          "of": {
            "kind": "string"
          }
        }
      },
      {
        "name": "working_directory",
        "optional": false,
        "schema": {
          "kind": "string"
        }
      },
      {
        "name": "user",
        "optional": false,
        "schema": {
          "kind": "string"
        }
      }
    ],
    "kind": "struct",
    "serde": {}
  },
  "ImagePruneResult": {
    "fields": [
      {
        "name": "deleted",
        "optional": false,
        "schema": {
          "bits": 64,
          "kind": "integer",
          "maximum": 9007199254740991,
          "minimum": 0,
          "signed": false
        }
      },
      {
        "name": "space_reclaimed",
        "optional": false,
        "schema": {
          "bits": 64,
          "kind": "integer",
          "maximum": 9007199254740991,
          "minimum": 0,
          "signed": false
        }
      }
    ],
    "kind": "struct",
    "serde": {}
  },
  "ImagePullChange": {
    "fields": [
      {
        "name": "job",
        "optional": false,
        "schema": {
          "kind": "string"
        }
      },
      {
        "name": "revision",
        "optional": false,
        "schema": {
          "bits": 64,
          "kind": "integer",
          "maximum": 9007199254740991,
          "minimum": 0,
          "signed": false
        }
      },
      {
        "name": "state",
        "optional": false,
        "schema": {
          "kind": "string"
        }
      },
      {
        "name": "coalesced",
        "optional": false,
        "schema": {
          "bits": 64,
          "kind": "integer",
          "maximum": 9007199254740991,
          "minimum": 0,
          "signed": false
        }
      }
    ],
    "kind": "struct",
    "serde": {}
  },
  "ImagePullJob": {
    "fields": [
      {
        "name": "job",
        "optional": false,
        "schema": {
          "kind": "string"
        }
      }
    ],
    "kind": "struct",
    "serde": {}
  },
  "ImagePullStatus": {
    "fields": [
      {
        "name": "job",
        "optional": false,
        "schema": {
          "kind": "string"
        }
      },
      {
        "name": "reference",
        "optional": false,
        "schema": {
          "kind": "string"
        }
      },
      {
        "name": "revision",
        "optional": false,
        "schema": {
          "bits": 64,
          "kind": "integer",
          "maximum": 9007199254740991,
          "minimum": 0,
          "signed": false
        }
      },
      {
        "name": "state",
        "optional": false,
        "schema": {
          "kind": "string"
        }
      },
      {
        "name": "status",
        "optional": true,
        "schema": {
          "kind": "optional",
          "of": {
            "kind": "string"
          }
        }
      },
      {
        "name": "layer",
        "optional": true,
        "schema": {
          "kind": "optional",
          "of": {
            "kind": "string"
          }
        }
      },
      {
        "name": "current",
        "optional": true,
        "schema": {
          "kind": "optional",
          "of": {
            "bits": 64,
            "kind": "integer",
            "maximum": 9007199254740991,
            "minimum": 0,
            "signed": false
          }
        }
      },
      {
        "name": "total",
        "optional": true,
        "schema": {
          "kind": "optional",
          "of": {
            "bits": 64,
            "kind": "integer",
            "maximum": 9007199254740991,
            "minimum": 0,
            "signed": false
          }
        }
      },
      {
        "name": "image",
        "optional": true,
        "schema": {
          "kind": "optional",
          "of": {
            "kind": "ref",
            "name": "ImageSummary"
          }
        }
      },
      {
        "name": "error",
        "optional": true,
        "schema": {
          "kind": "optional",
          "of": {
            "kind": "string"
          }
        }
      }
    ],
    "kind": "struct",
    "serde": {}
  },
  "ImageSummary": {
    "fields": [
      {
        "name": "id",
        "optional": false,
        "schema": {
          "kind": "string"
        }
      },
      {
        "name": "reference",
        "optional": false,
        "schema": {
          "kind": "string"
        }
      },
      {
        "name": "size",
        "optional": false,
        "schema": {
          "bits": 64,
          "kind": "integer",
          "maximum": 9007199254740991,
          "minimum": 0,
          "signed": false
        }
      },
      {
        "name": "created",
        "optional": false,
        "schema": {
          "bits": 64,
          "kind": "integer",
          "maximum": 9007199254740991,
          "minimum": -9007199254740991,
          "signed": true
        }
      }
    ],
    "kind": "struct",
    "serde": {}
  },
  "InspectablePane": {
    "fields": [
      {
        "name": "slot",
        "optional": false,
        "schema": {
          "kind": "string"
        }
      },
      {
        "name": "generation",
        "optional": true,
        "schema": {
          "bits": 64,
          "kind": "integer",
          "maximum": 9007199254740991,
          "minimum": 0,
          "signed": false
        }
      },
      {
        "name": "revision",
        "optional": true,
        "schema": {
          "bits": 64,
          "kind": "integer",
          "maximum": 9007199254740991,
          "minimum": 0,
          "signed": false
        }
      },
      {
        "name": "kind",
        "optional": false,
        "schema": {
          "kind": "ref",
          "name": "PaneKind"
        }
      },
      {
        "name": "provider",
        "optional": true,
        "schema": {
          "kind": "optional",
          "of": {
            "kind": "ref",
            "name": "PaneProviderIdentity"
          }
        }
      },
      {
        "name": "tab",
        "optional": true,
        "schema": {
          "kind": "optional",
          "of": {
            "kind": "string"
          }
        }
      },
      {
        "name": "title",
        "optional": true,
        "schema": {
          "kind": "optional",
          "of": {
            "kind": "string"
          }
        }
      },
      {
        "name": "focused",
        "optional": false,
        "schema": {
          "kind": "boolean"
        }
      }
    ],
    "kind": "struct",
    "serde": {}
  },
  "LayoutNode": {
    "kind": "enum",
    "serde": {
      "rename_all": "kebab-case",
      "tag": "kind"
    },
    "variants": [
      {
        "name": "pane",
        "payload": {
          "fields": [
            {
              "name": "pane",
              "optional": false,
              "schema": {
                "kind": "ref",
                "name": "PaneSummary"
              }
            },
            {
              "name": "grid",
              "optional": true,
              "schema": {
                "kind": "optional",
                "of": {
                  "kind": "ref",
                  "name": "GridSize"
                }
              }
            },
            {
              "name": "focused",
              "optional": false,
              "schema": {
                "kind": "boolean"
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "split",
        "payload": {
          "fields": [
            {
              "name": "division",
              "optional": false,
              "schema": {
                "kind": "ref",
                "name": "Division"
              }
            },
            {
              "name": "ratio_per_mille",
              "optional": false,
              "schema": {
                "bits": 16,
                "kind": "integer",
                "maximum": 65535,
                "minimum": 0,
                "signed": false
              }
            },
            {
              "name": "first",
              "optional": false,
              "schema": {
                "kind": "ref",
                "name": "LayoutNode"
              }
            },
            {
              "name": "second",
              "optional": false,
              "schema": {
                "kind": "ref",
                "name": "LayoutNode"
              }
            }
          ],
          "kind": "struct"
        }
      }
    ]
  },
  "Length": {
    "kind": "enum",
    "serde": {},
    "variants": [
      {
        "name": "Step",
        "payload": {
          "kind": "newtype",
          "of": {
            "bits": 8,
            "kind": "integer",
            "maximum": 255,
            "minimum": 0,
            "signed": false
          }
        }
      },
      {
        "name": "Chars",
        "payload": {
          "kind": "newtype",
          "of": {
            "bits": 16,
            "kind": "integer",
            "maximum": 65535,
            "minimum": 0,
            "signed": false
          }
        }
      },
      {
        "name": "Fill",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Content",
        "payload": {
          "kind": "unit"
        }
      }
    ]
  },
  "NetworkSummary": {
    "fields": [
      {
        "name": "id",
        "optional": false,
        "schema": {
          "kind": "string"
        }
      },
      {
        "name": "name",
        "optional": false,
        "schema": {
          "kind": "string"
        }
      },
      {
        "name": "driver",
        "optional": false,
        "schema": {
          "kind": "string"
        }
      },
      {
        "name": "scope",
        "optional": false,
        "schema": {
          "kind": "string"
        }
      }
    ],
    "kind": "struct",
    "serde": {}
  },
  "NodeId": {
    "kind": "newtype",
    "of": {
      "bits": 64,
      "kind": "integer",
      "maximum": 9007199254740991,
      "minimum": 0,
      "signed": false
    },
    "serde": {}
  },
  "Occupant": {
    "kind": "enum",
    "serde": {
      "rename_all": "kebab-case"
    },
    "variants": [
      {
        "name": "terminal",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "surface",
        "payload": {
          "kind": "unit"
        }
      }
    ]
  },
  "Orientation": {
    "kind": "enum",
    "serde": {},
    "variants": [
      {
        "name": "Horizontal",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Vertical",
        "payload": {
          "kind": "unit"
        }
      }
    ]
  },
  "PaneChange": {
    "fields": [
      {
        "name": "slot",
        "optional": false,
        "schema": {
          "kind": "string"
        }
      },
      {
        "name": "kind",
        "optional": false,
        "schema": {
          "kind": "ref",
          "name": "PaneChangeKind"
        }
      },
      {
        "name": "revision",
        "optional": false,
        "schema": {
          "bits": 64,
          "kind": "integer",
          "maximum": 9007199254740991,
          "minimum": 0,
          "signed": false
        }
      },
      {
        "name": "generation",
        "optional": false,
        "schema": {
          "bits": 64,
          "kind": "integer",
          "maximum": 9007199254740991,
          "minimum": 0,
          "signed": false
        }
      },
      {
        "name": "coalesced",
        "optional": false,
        "schema": {
          "bits": 64,
          "kind": "integer",
          "maximum": 9007199254740991,
          "minimum": 0,
          "signed": false
        }
      }
    ],
    "kind": "struct",
    "serde": {}
  },
  "PaneChangeKind": {
    "kind": "enum",
    "serde": {
      "rename_all": "snake_case"
    },
    "variants": [
      {
        "name": "terminal",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "surface",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "native",
        "payload": {
          "kind": "unit"
        }
      }
    ]
  },
  "PaneInventory": {
    "fields": [
      {
        "name": "panes",
        "optional": false,
        "schema": {
          "kind": "array",
          "of": {
            "kind": "ref",
            "name": "InspectablePane"
          }
        }
      },
      {
        "name": "truncated",
        "optional": false,
        "schema": {
          "kind": "boolean"
        }
      }
    ],
    "kind": "struct",
    "serde": {}
  },
  "PaneKind": {
    "kind": "enum",
    "serde": {
      "rename_all": "kebab-case"
    },
    "variants": [
      {
        "name": "terminal",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "surface",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "native",
        "payload": {
          "kind": "unit"
        }
      }
    ]
  },
  "PaneOccupantTarget": {
    "kind": "enum",
    "serde": {
      "rename_all": "kebab-case",
      "tag": "kind"
    },
    "variants": [
      {
        "name": "terminal",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "surface",
        "payload": {
          "fields": [
            {
              "name": "extension",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            },
            {
              "name": "provider",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            }
          ],
          "kind": "struct"
        }
      }
    ]
  },
  "PaneProvider": {
    "fields": [
      {
        "name": "id",
        "optional": false,
        "schema": {
          "kind": "ref",
          "name": "ExtensionName"
        }
      },
      {
        "name": "title",
        "optional": false,
        "schema": {
          "kind": "string"
        }
      },
      {
        "name": "icon",
        "optional": true,
        "schema": {
          "kind": "optional",
          "of": {
            "kind": "string"
          }
        }
      }
    ],
    "kind": "struct",
    "serde": {
      "deny_unknown_fields": true
    }
  },
  "PaneProviderIdentity": {
    "fields": [
      {
        "name": "extension",
        "optional": false,
        "schema": {
          "kind": "string"
        }
      },
      {
        "name": "provider",
        "optional": false,
        "schema": {
          "kind": "string"
        }
      }
    ],
    "kind": "struct",
    "serde": {}
  },
  "PaneSemanticAction": {
    "fields": [
      {
        "name": "generation",
        "optional": false,
        "schema": {
          "bits": 64,
          "kind": "integer",
          "maximum": 9007199254740991,
          "minimum": 0,
          "signed": false
        }
      },
      {
        "name": "revision",
        "optional": false,
        "schema": {
          "bits": 64,
          "kind": "integer",
          "maximum": 9007199254740991,
          "minimum": 0,
          "signed": false
        }
      },
      {
        "name": "node",
        "optional": false,
        "schema": {
          "bits": 64,
          "kind": "integer",
          "maximum": 9007199254740991,
          "minimum": 0,
          "signed": false
        }
      },
      {
        "name": "action",
        "optional": false,
        "schema": {
          "kind": "ref",
          "name": "SemanticActionKind"
        }
      },
      {
        "name": "value",
        "optional": true,
        "schema": {
          "kind": "optional",
          "of": {
            "kind": "string"
          }
        }
      }
    ],
    "kind": "struct",
    "serde": {}
  },
  "PaneSemanticTree": {
    "fields": [
      {
        "name": "slot",
        "optional": false,
        "schema": {
          "kind": "string"
        }
      },
      {
        "name": "generation",
        "optional": false,
        "schema": {
          "bits": 64,
          "kind": "integer",
          "maximum": 9007199254740991,
          "minimum": 0,
          "signed": false
        }
      },
      {
        "name": "revision",
        "optional": false,
        "schema": {
          "bits": 64,
          "kind": "integer",
          "maximum": 9007199254740991,
          "minimum": 0,
          "signed": false
        }
      },
      {
        "name": "root",
        "optional": false,
        "schema": {
          "kind": "ref",
          "name": "SemanticNode"
        }
      },
      {
        "name": "truncated",
        "optional": false,
        "schema": {
          "kind": "boolean"
        }
      }
    ],
    "kind": "struct",
    "serde": {}
  },
  "PaneSummary": {
    "fields": [
      {
        "name": "slot",
        "optional": false,
        "schema": {
          "kind": "string"
        }
      },
      {
        "name": "working_directory",
        "optional": true,
        "schema": {
          "kind": "optional",
          "of": {
            "kind": "string"
          }
        }
      },
      {
        "name": "command",
        "optional": true,
        "schema": {
          "kind": "optional",
          "of": {
            "kind": "string"
          }
        }
      },
      {
        "name": "occupant",
        "optional": false,
        "schema": {
          "kind": "ref",
          "name": "Occupant"
        }
      },
      {
        "name": "provider",
        "optional": true,
        "schema": {
          "kind": "optional",
          "of": {
            "kind": "ref",
            "name": "PaneProviderIdentity"
          }
        }
      }
    ],
    "kind": "struct",
    "serde": {}
  },
  "PaneText": {
    "fields": [
      {
        "name": "slot",
        "optional": false,
        "schema": {
          "kind": "string"
        }
      },
      {
        "name": "generation",
        "optional": true,
        "schema": {
          "bits": 64,
          "kind": "integer",
          "maximum": 9007199254740991,
          "minimum": 0,
          "signed": false
        }
      },
      {
        "name": "revision",
        "optional": true,
        "schema": {
          "bits": 64,
          "kind": "integer",
          "maximum": 9007199254740991,
          "minimum": 0,
          "signed": false
        }
      },
      {
        "name": "columns",
        "optional": true,
        "schema": {
          "bits": 16,
          "kind": "integer",
          "maximum": 65535,
          "minimum": 0,
          "signed": false
        }
      },
      {
        "name": "rows",
        "optional": true,
        "schema": {
          "bits": 16,
          "kind": "integer",
          "maximum": 65535,
          "minimum": 0,
          "signed": false
        }
      },
      {
        "name": "lines",
        "optional": false,
        "schema": {
          "kind": "array",
          "of": {
            "kind": "string"
          }
        }
      },
      {
        "name": "cursor_column",
        "optional": true,
        "schema": {
          "bits": 32,
          "kind": "integer",
          "maximum": 4294967295,
          "minimum": 0,
          "signed": false
        }
      },
      {
        "name": "cursor_row",
        "optional": true,
        "schema": {
          "bits": 32,
          "kind": "integer",
          "maximum": 4294967295,
          "minimum": 0,
          "signed": false
        }
      },
      {
        "name": "truncated",
        "optional": false,
        "schema": {
          "kind": "boolean"
        }
      }
    ],
    "kind": "struct",
    "serde": {}
  },
  "Patch": {
    "kind": "enum",
    "serde": {},
    "variants": [
      {
        "name": "Create",
        "payload": {
          "fields": [
            {
              "name": "id",
              "optional": false,
              "schema": {
                "kind": "ref",
                "name": "NodeId"
              }
            },
            {
              "name": "tag",
              "optional": false,
              "schema": {
                "kind": "ref",
                "name": "Tag"
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "Insert",
        "payload": {
          "fields": [
            {
              "name": "parent",
              "optional": false,
              "schema": {
                "kind": "ref",
                "name": "NodeId"
              }
            },
            {
              "name": "child",
              "optional": false,
              "schema": {
                "kind": "ref",
                "name": "NodeId"
              }
            },
            {
              "name": "before",
              "optional": true,
              "schema": {
                "kind": "optional",
                "of": {
                  "kind": "ref",
                  "name": "NodeId"
                }
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "Move",
        "payload": {
          "fields": [
            {
              "name": "parent",
              "optional": false,
              "schema": {
                "kind": "ref",
                "name": "NodeId"
              }
            },
            {
              "name": "child",
              "optional": false,
              "schema": {
                "kind": "ref",
                "name": "NodeId"
              }
            },
            {
              "name": "before",
              "optional": true,
              "schema": {
                "kind": "optional",
                "of": {
                  "kind": "ref",
                  "name": "NodeId"
                }
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "SetProp",
        "payload": {
          "fields": [
            {
              "name": "id",
              "optional": false,
              "schema": {
                "kind": "ref",
                "name": "NodeId"
              }
            },
            {
              "name": "prop",
              "optional": false,
              "schema": {
                "kind": "ref",
                "name": "Prop"
              }
            },
            {
              "name": "value",
              "optional": false,
              "schema": {
                "kind": "ref",
                "name": "PropValue"
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "ClearProp",
        "payload": {
          "fields": [
            {
              "name": "id",
              "optional": false,
              "schema": {
                "kind": "ref",
                "name": "NodeId"
              }
            },
            {
              "name": "prop",
              "optional": false,
              "schema": {
                "kind": "ref",
                "name": "Prop"
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "SetHandler",
        "payload": {
          "fields": [
            {
              "name": "id",
              "optional": false,
              "schema": {
                "kind": "ref",
                "name": "NodeId"
              }
            },
            {
              "name": "handler",
              "optional": false,
              "schema": {
                "kind": "ref",
                "name": "Handler"
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "ClearHandler",
        "payload": {
          "fields": [
            {
              "name": "id",
              "optional": false,
              "schema": {
                "kind": "ref",
                "name": "NodeId"
              }
            },
            {
              "name": "trigger",
              "optional": false,
              "schema": {
                "kind": "ref",
                "name": "Trigger"
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "Remove",
        "payload": {
          "fields": [
            {
              "name": "id",
              "optional": false,
              "schema": {
                "kind": "ref",
                "name": "NodeId"
              }
            }
          ],
          "kind": "struct"
        }
      }
    ]
  },
  "PeerName": {
    "kind": "string"
  },
  "PointerPhase": {
    "kind": "enum",
    "serde": {
      "rename_all": "snake_case"
    },
    "variants": [
      {
        "name": "move",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "enter",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "leave",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "press",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "release",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "click",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "context",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "scroll",
        "payload": {
          "kind": "unit"
        }
      }
    ]
  },
  "ProcessList": {
    "fields": [
      {
        "name": "container_id",
        "optional": true,
        "schema": {
          "kind": "string"
        }
      },
      {
        "name": "titles",
        "optional": false,
        "schema": {
          "kind": "array",
          "of": {
            "kind": "string"
          }
        }
      },
      {
        "name": "processes",
        "optional": false,
        "schema": {
          "kind": "array",
          "of": {
            "kind": "array",
            "of": {
              "kind": "string"
            }
          }
        }
      },
      {
        "name": "observed_at_ms",
        "optional": true,
        "schema": {
          "bits": 64,
          "kind": "integer",
          "maximum": 9007199254740991,
          "minimum": 0,
          "signed": false
        }
      },
      {
        "name": "scope",
        "optional": true,
        "schema": {
          "kind": "ref",
          "name": "ProcessScope"
        }
      },
      {
        "name": "pid_identity",
        "optional": true,
        "schema": {
          "kind": "ref",
          "name": "ProcessPidIdentity"
        }
      },
      {
        "name": "truncated",
        "optional": true,
        "schema": {
          "kind": "boolean"
        }
      }
    ],
    "kind": "struct",
    "serde": {}
  },
  "ProcessPidIdentity": {
    "kind": "enum",
    "serde": {
      "rename_all": "snake_case"
    },
    "variants": [
      {
        "name": "snapshot",
        "payload": {
          "kind": "unit"
        }
      }
    ]
  },
  "ProcessScope": {
    "kind": "enum",
    "serde": {
      "rename_all": "snake_case"
    },
    "variants": [
      {
        "name": "initial",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "namespace",
        "payload": {
          "kind": "unit"
        }
      }
    ]
  },
  "Prop": {
    "kind": "enum",
    "serde": {},
    "variants": [
      {
        "name": "Label",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Detail",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Value",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Placeholder",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Help",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Icon",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Tooltip",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Uri",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Enabled",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Visible",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Selected",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Checked",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Expanded",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Busy",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Secret",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Destructive",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Monospace",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Wrap",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Ellipsize",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Variant",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Tone",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Scale",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Color",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Gap",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Pad",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Grow",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Width",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Height",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Align",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Justify",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Columns",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Span",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "RowSpan",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Orientation",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Position",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Minimum",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Maximum",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Step",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Fraction",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Schema",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Source",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "RowHeight",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Choices",
        "payload": {
          "kind": "unit"
        }
      }
    ]
  },
  "PropValue": {
    "kind": "enum",
    "serde": {},
    "variants": [
      {
        "name": "Text",
        "payload": {
          "kind": "newtype",
          "of": {
            "kind": "string"
          }
        }
      },
      {
        "name": "Number",
        "payload": {
          "kind": "newtype",
          "of": {
            "bits": 64,
            "kind": "float"
          }
        }
      },
      {
        "name": "Integer",
        "payload": {
          "kind": "newtype",
          "of": {
            "bits": 64,
            "kind": "integer",
            "maximum": 9007199254740991,
            "minimum": -9007199254740991,
            "signed": true
          }
        }
      },
      {
        "name": "Flag",
        "payload": {
          "kind": "newtype",
          "of": {
            "kind": "boolean"
          }
        }
      },
      {
        "name": "Token",
        "payload": {
          "kind": "newtype",
          "of": {
            "kind": "ref",
            "name": "Token"
          }
        }
      },
      {
        "name": "Length",
        "payload": {
          "kind": "newtype",
          "of": {
            "kind": "ref",
            "name": "Length"
          }
        }
      },
      {
        "name": "Edges",
        "payload": {
          "kind": "newtype",
          "of": {
            "kind": "ref",
            "name": "Edges"
          }
        }
      },
      {
        "name": "Bounds",
        "payload": {
          "kind": "newtype",
          "of": {
            "kind": "ref",
            "name": "Bounds"
          }
        }
      },
      {
        "name": "Variant",
        "payload": {
          "kind": "newtype",
          "of": {
            "kind": "ref",
            "name": "Variant"
          }
        }
      },
      {
        "name": "Tone",
        "payload": {
          "kind": "newtype",
          "of": {
            "kind": "ref",
            "name": "Tone"
          }
        }
      },
      {
        "name": "Scale",
        "payload": {
          "kind": "newtype",
          "of": {
            "kind": "ref",
            "name": "Scale"
          }
        }
      },
      {
        "name": "Align",
        "payload": {
          "kind": "newtype",
          "of": {
            "kind": "ref",
            "name": "Align"
          }
        }
      },
      {
        "name": "Orientation",
        "payload": {
          "kind": "newtype",
          "of": {
            "kind": "ref",
            "name": "Orientation"
          }
        }
      },
      {
        "name": "Choices",
        "payload": {
          "kind": "newtype",
          "of": {
            "kind": "array",
            "of": {
              "kind": "ref",
              "name": "Choice"
            }
          }
        }
      },
      {
        "name": "Schema",
        "payload": {
          "kind": "newtype",
          "of": {
            "kind": "array",
            "of": {
              "kind": "ref",
              "name": "Column"
            }
          }
        }
      },
      {
        "name": "Source",
        "payload": {
          "kind": "newtype",
          "of": {
            "kind": "ref",
            "name": "SourceId"
          }
        }
      },
      {
        "name": "Nothing",
        "payload": {
          "kind": "unit"
        }
      }
    ]
  },
  "RelativePath": {
    "kind": "string"
  },
  "RequestId": {
    "kind": "newtype",
    "of": {
      "bits": 64,
      "kind": "integer",
      "maximum": 9007199254740991,
      "minimum": 0,
      "signed": false
    },
    "serde": {}
  },
  "Row": {
    "fields": [
      {
        "name": "key",
        "optional": false,
        "schema": {
          "bits": 64,
          "kind": "integer",
          "maximum": 9007199254740991,
          "minimum": 0,
          "signed": false
        }
      },
      {
        "name": "cells",
        "optional": false,
        "schema": {
          "kind": "array",
          "of": {
            "kind": "ref",
            "name": "Cell"
          }
        }
      }
    ],
    "kind": "struct",
    "serde": {}
  },
  "RowRange": {
    "fields": [
      {
        "name": "start",
        "optional": false,
        "schema": {
          "bits": 64,
          "kind": "integer",
          "maximum": 9007199254740991,
          "minimum": 0,
          "signed": false
        }
      },
      {
        "name": "count",
        "optional": false,
        "schema": {
          "bits": 32,
          "kind": "integer",
          "maximum": 4294967295,
          "minimum": 0,
          "signed": false
        }
      }
    ],
    "kind": "struct",
    "serde": {}
  },
  "RowWindow": {
    "fields": [
      {
        "name": "source",
        "optional": false,
        "schema": {
          "kind": "ref",
          "name": "SourceId"
        }
      },
      {
        "name": "version",
        "optional": false,
        "schema": {
          "kind": "ref",
          "name": "Version"
        }
      },
      {
        "name": "request",
        "optional": false,
        "schema": {
          "kind": "ref",
          "name": "RequestId"
        }
      },
      {
        "name": "range",
        "optional": false,
        "schema": {
          "kind": "ref",
          "name": "RowRange"
        }
      },
      {
        "name": "rows",
        "optional": false,
        "schema": {
          "kind": "array",
          "of": {
            "kind": "ref",
            "name": "Row"
          }
        }
      }
    ],
    "kind": "struct",
    "serde": {}
  },
  "Scale": {
    "kind": "enum",
    "serde": {},
    "variants": [
      {
        "name": "Caption",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Body",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Title",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Display",
        "payload": {
          "kind": "unit"
        }
      }
    ]
  },
  "SemanticActionKind": {
    "kind": "enum",
    "serde": {
      "rename_all": "snake_case"
    },
    "variants": [
      {
        "name": "invoke",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "change",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "submit",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "toggle",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "expand",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "focus",
        "payload": {
          "kind": "unit"
        }
      }
    ]
  },
  "SemanticNode": {
    "fields": [
      {
        "name": "id",
        "optional": false,
        "schema": {
          "bits": 64,
          "kind": "integer",
          "maximum": 9007199254740991,
          "minimum": 0,
          "signed": false
        }
      },
      {
        "name": "role",
        "optional": false,
        "schema": {
          "kind": "string"
        }
      },
      {
        "name": "label",
        "optional": true,
        "schema": {
          "kind": "optional",
          "of": {
            "kind": "string"
          }
        }
      },
      {
        "name": "value",
        "optional": true,
        "schema": {
          "kind": "optional",
          "of": {
            "kind": "string"
          }
        }
      },
      {
        "name": "disabled",
        "optional": false,
        "schema": {
          "kind": "boolean"
        }
      },
      {
        "name": "destructive",
        "optional": false,
        "schema": {
          "kind": "boolean"
        }
      },
      {
        "name": "actions",
        "optional": false,
        "schema": {
          "kind": "array",
          "of": {
            "kind": "ref",
            "name": "SemanticActionKind"
          }
        }
      },
      {
        "name": "children",
        "optional": false,
        "schema": {
          "kind": "array",
          "of": {
            "kind": "ref",
            "name": "SemanticNode"
          }
        }
      }
    ],
    "kind": "struct",
    "serde": {}
  },
  "SourceId": {
    "kind": "newtype",
    "of": {
      "bits": 64,
      "kind": "integer",
      "maximum": 9007199254740991,
      "minimum": 0,
      "signed": false
    },
    "serde": {}
  },
  "SourceMutation": {
    "kind": "enum",
    "serde": {},
    "variants": [
      {
        "name": "Open",
        "payload": {
          "fields": [
            {
              "name": "source",
              "optional": false,
              "schema": {
                "kind": "ref",
                "name": "SourceId"
              }
            },
            {
              "name": "columns",
              "optional": false,
              "schema": {
                "kind": "array",
                "of": {
                  "kind": "ref",
                  "name": "Column"
                }
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "Length",
        "payload": {
          "fields": [
            {
              "name": "source",
              "optional": false,
              "schema": {
                "kind": "ref",
                "name": "SourceId"
              }
            },
            {
              "name": "version",
              "optional": false,
              "schema": {
                "kind": "ref",
                "name": "Version"
              }
            },
            {
              "name": "rows",
              "optional": false,
              "schema": {
                "bits": 64,
                "kind": "integer",
                "maximum": 9007199254740991,
                "minimum": 0,
                "signed": false
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "Window",
        "payload": {
          "kind": "newtype",
          "of": {
            "kind": "ref",
            "name": "RowWindow"
          }
        }
      },
      {
        "name": "Invalidate",
        "payload": {
          "fields": [
            {
              "name": "source",
              "optional": false,
              "schema": {
                "kind": "ref",
                "name": "SourceId"
              }
            },
            {
              "name": "version",
              "optional": false,
              "schema": {
                "kind": "ref",
                "name": "Version"
              }
            },
            {
              "name": "range",
              "optional": true,
              "schema": {
                "kind": "optional",
                "of": {
                  "kind": "ref",
                  "name": "RowRange"
                }
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "Close",
        "payload": {
          "fields": [
            {
              "name": "source",
              "optional": false,
              "schema": {
                "kind": "ref",
                "name": "SourceId"
              }
            }
          ],
          "kind": "struct"
        }
      }
    ]
  },
  "TabSummary": {
    "fields": [
      {
        "name": "id",
        "optional": false,
        "schema": {
          "kind": "string"
        }
      },
      {
        "name": "title",
        "optional": false,
        "schema": {
          "kind": "string"
        }
      },
      {
        "name": "panes",
        "optional": false,
        "schema": {
          "kind": "array",
          "of": {
            "kind": "ref",
            "name": "PaneSummary"
          }
        }
      }
    ],
    "kind": "struct",
    "serde": {}
  },
  "TabTopology": {
    "fields": [
      {
        "name": "id",
        "optional": false,
        "schema": {
          "kind": "string"
        }
      },
      {
        "name": "title",
        "optional": false,
        "schema": {
          "kind": "string"
        }
      },
      {
        "name": "root",
        "optional": false,
        "schema": {
          "kind": "ref",
          "name": "LayoutNode"
        }
      }
    ],
    "kind": "struct",
    "serde": {}
  },
  "Tag": {
    "kind": "enum",
    "serde": {},
    "variants": [
      {
        "name": "Column",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Row",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Grid",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Scroll",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Splitter",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Stack",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Overlay",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Container",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Spacer",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Separator",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Card",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "CardHeader",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "CardContent",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "CardActions",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "CardMedia",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "CardActionArea",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Paper",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Section",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Toolbar",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "HeaderBar",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Sidebar",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Text",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Heading",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Code",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Link",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Icon",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Badge",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Avatar",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "AvatarGroup",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Chip",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Image",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "ImageList",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "ImageListItem",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Progress",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Spinner",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Meter",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Skeleton",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "EmptyState",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Stat",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Toast",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Banner",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "AlertTitle",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "InlineMessage",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "ValidationSummary",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Button",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "IconButton",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "ToggleButton",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "ButtonGroup",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "ToggleButtonGroup",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "SplitButton",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Fab",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "SpeedDial",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "SpeedDialAction",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Overflow",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Entry",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Search",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "CommandPalette",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "TagInput",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "NumberEntry",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "TextArea",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "PasswordEntry",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Autocomplete",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "TextField",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "InputAdornment",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Slider",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "DatePicker",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "TimePicker",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "ColorPicker",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "FilePicker",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Rating",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "FormControl",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "FormLabel",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "FormHelperText",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "FormControlLabel",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "FormGroup",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Switch",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Checkbox",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Radio",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "RadioGroup",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Select",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "List",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "ListRow",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "ListItemText",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "ListItemIcon",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "ListItemAvatar",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "ListItemButton",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "ListItemAction",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "ListItemSecondaryAction",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "ListSubheader",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Table",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "TableHead",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "TableBody",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "TableFooter",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "TableRow",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "TableCell",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "TableSortLabel",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "DataTable",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "KeyValueTable",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "TreeTable",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "EventStream",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "FileBrowser",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "TablePagination",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Tree",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "TreeItem",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Tabs",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "TabPage",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Breadcrumb",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Pagination",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "PaginationItem",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Stepper",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Step",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "StepLabel",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "StepContent",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "StepConnector",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "StepIcon",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "NavigationRail",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "NavigationRailItem",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "BottomNavigation",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "BottomNavigationAction",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Accordion",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "AccordionSummary",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "AccordionDetails",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "AccordionActions",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Expander",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Dialog",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "DialogTitle",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "DialogContent",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "DialogContentText",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "DialogActions",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Popover",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "ContextMenu",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Menu",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "MenuItem",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Drawer",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "DrawerPanel",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "CodeView",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "HexView",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "MarkdownView",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "JsonView",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "LogView",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Video",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Chart",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Sparkline",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "FlameGraph",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "MemoryMap",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "DisassemblyView",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "TimelineView",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "TestReportView",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "CoverageView",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "NetworkWaterfall",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "NetworkRequest",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "NetworkPhase",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "DependencyGraph",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "DependencyNode",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "DependencyEdge",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "DependencyCycle",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "DependencyCycleMember",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "QueryPlan",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "QueryPlanNode",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "QueryPlanMetric",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "DiffViewer",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "DiffLine",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "StackTrace",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "StackFrame",
        "payload": {
          "kind": "unit"
        }
      }
    ]
  },
  "TerminalTopology": {
    "fields": [
      {
        "name": "active_tab",
        "optional": true,
        "schema": {
          "kind": "optional",
          "of": {
            "kind": "string"
          }
        }
      },
      {
        "name": "tabs",
        "optional": false,
        "schema": {
          "kind": "array",
          "of": {
            "kind": "ref",
            "name": "TabTopology"
          }
        }
      }
    ],
    "kind": "struct",
    "serde": {}
  },
  "Token": {
    "kind": "enum",
    "serde": {},
    "variants": [
      {
        "name": "Ground",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Surface",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Raised",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Line",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Text",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "TextDim",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "TextFaint",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Accent",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Positive",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Warning",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Danger",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Info",
        "payload": {
          "kind": "unit"
        }
      }
    ]
  },
  "Tone": {
    "kind": "enum",
    "serde": {},
    "variants": [
      {
        "name": "Neutral",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Accent",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Positive",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Warning",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Danger",
        "payload": {
          "kind": "unit"
        }
      }
    ]
  },
  "Topic": {
    "kind": "enum",
    "serde": {
      "rename_all": "kebab-case"
    },
    "variants": [
      {
        "name": "containers",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "container-inventory",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "executions",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "images",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "image-pulls",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "volumes",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "networks",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "terminal",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "pane-changes",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "extensions",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "extension-acquisitions",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "workspace-lifecycle",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "workspace-events",
        "payload": {
          "kind": "unit"
        }
      }
    ]
  },
  "Trigger": {
    "kind": "enum",
    "serde": {},
    "variants": [
      {
        "name": "Invoke",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Change",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Submit",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Select",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Edit",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Sort",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Activate",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Toggle",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Expand",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Scroll",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Close",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Context",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Key",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Focus",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Pointer",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Drag",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Drop",
        "payload": {
          "kind": "unit"
        }
      }
    ]
  },
  "UiCollectionSelection": {
    "fields": [
      {
        "name": "source",
        "optional": false,
        "schema": {
          "bits": 64,
          "kind": "integer",
          "maximum": 9007199254740991,
          "minimum": 0,
          "signed": false
        }
      },
      {
        "name": "version",
        "optional": false,
        "schema": {
          "bits": 64,
          "kind": "integer",
          "maximum": 9007199254740991,
          "minimum": 0,
          "signed": false
        }
      },
      {
        "name": "rows",
        "optional": false,
        "schema": {
          "kind": "array",
          "of": {
            "kind": "ref",
            "name": "UiSelectedRow"
          }
        }
      }
    ],
    "kind": "struct",
    "serde": {}
  },
  "UiPointerPhase": {
    "kind": "enum",
    "serde": {},
    "variants": [
      {
        "name": "enter",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "motion",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "leave",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "press",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "release",
        "payload": {
          "kind": "unit"
        }
      }
    ]
  },
  "UiSelectedRow": {
    "fields": [
      {
        "name": "index",
        "optional": false,
        "schema": {
          "bits": 64,
          "kind": "integer",
          "maximum": 9007199254740991,
          "minimum": 0,
          "signed": false
        }
      },
      {
        "name": "id",
        "optional": false,
        "schema": {
          "kind": "string"
        }
      }
    ],
    "kind": "struct",
    "serde": {}
  },
  "Variant": {
    "kind": "enum",
    "serde": {},
    "variants": [
      {
        "name": "Plain",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Filled",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Outline",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "Ghost",
        "payload": {
          "kind": "unit"
        }
      }
    ]
  },
  "Version": {
    "kind": "newtype",
    "of": {
      "bits": 64,
      "kind": "integer",
      "maximum": 9007199254740991,
      "minimum": 0,
      "signed": false
    },
    "serde": {}
  },
  "VolumeSummary": {
    "fields": [
      {
        "name": "name",
        "optional": false,
        "schema": {
          "kind": "string"
        }
      },
      {
        "name": "driver",
        "optional": false,
        "schema": {
          "kind": "string"
        }
      },
      {
        "name": "generation",
        "optional": false,
        "schema": {
          "kind": "string"
        }
      }
    ],
    "kind": "struct",
    "serde": {}
  },
  "WorkspaceConfiguration": {
    "fields": [
      {
        "name": "generation",
        "optional": true,
        "schema": {
          "kind": "string"
        }
      },
      {
        "name": "name",
        "optional": false,
        "schema": {
          "kind": "string"
        }
      },
      {
        "name": "image",
        "optional": false,
        "schema": {
          "kind": "string"
        }
      },
      {
        "name": "architecture",
        "optional": false,
        "schema": {
          "kind": "string"
        }
      },
      {
        "name": "storage",
        "optional": true,
        "schema": {
          "kind": "optional",
          "of": {
            "kind": "string"
          }
        }
      },
      {
        "name": "shell",
        "optional": true,
        "schema": {
          "kind": "optional",
          "of": {
            "kind": "string"
          }
        }
      },
      {
        "name": "cpus",
        "optional": true,
        "schema": {
          "kind": "optional",
          "of": {
            "bits": 32,
            "kind": "integer",
            "maximum": 4294967295,
            "minimum": 0,
            "signed": false
          }
        }
      },
      {
        "name": "memory_mb",
        "optional": true,
        "schema": {
          "kind": "optional",
          "of": {
            "bits": 32,
            "kind": "integer",
            "maximum": 4294967295,
            "minimum": 0,
            "signed": false
          }
        }
      },
      {
        "name": "environment",
        "optional": false,
        "schema": {
          "kind": "array",
          "of": {
            "items": [
              {
                "kind": "string"
              },
              {
                "kind": "string"
              }
            ],
            "kind": "tuple"
          }
        }
      },
      {
        "name": "mounts",
        "optional": false,
        "schema": {
          "kind": "array",
          "of": {
            "kind": "ref",
            "name": "WorkspaceMount"
          }
        }
      },
      {
        "name": "docker_socket",
        "optional": false,
        "schema": {
          "kind": "boolean"
        }
      },
      {
        "name": "scrollback",
        "optional": true,
        "schema": {
          "kind": "optional",
          "of": {
            "bits": 64,
            "kind": "integer",
            "maximum": 9007199254740991,
            "minimum": 0,
            "signed": false
          }
        }
      },
      {
        "name": "vpn",
        "optional": true,
        "schema": {
          "kind": "optional",
          "of": {
            "kind": "string"
          }
        }
      },
      {
        "name": "execution_lifetime",
        "optional": false,
        "schema": {
          "kind": "string"
        }
      },
      {
        "name": "terminal",
        "optional": false,
        "schema": {
          "kind": "ref",
          "name": "WorkspaceTerminal"
        }
      }
    ],
    "kind": "struct",
    "serde": {}
  },
  "WorkspaceEvent": {
    "kind": "enum",
    "serde": {
      "rename_all": "snake_case",
      "tag": "event"
    },
    "variants": [
      {
        "name": "key",
        "payload": {
          "fields": [
            {
              "name": "key",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            },
            {
              "name": "modifiers",
              "optional": false,
              "schema": {
                "kind": "array",
                "of": {
                  "kind": "string"
                }
              }
            },
            {
              "name": "pressed",
              "optional": false,
              "schema": {
                "kind": "boolean"
              }
            },
            {
              "name": "slot",
              "optional": true,
              "schema": {
                "kind": "optional",
                "of": {
                  "kind": "string"
                }
              }
            },
            {
              "name": "generation",
              "optional": true,
              "schema": {
                "kind": "optional",
                "of": {
                  "bits": 64,
                  "kind": "integer",
                  "maximum": 9007199254740991,
                  "minimum": 0,
                  "signed": false
                }
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "focus",
        "payload": {
          "fields": [
            {
              "name": "active",
              "optional": false,
              "schema": {
                "kind": "boolean"
              }
            },
            {
              "name": "slot",
              "optional": true,
              "schema": {
                "kind": "optional",
                "of": {
                  "kind": "string"
                }
              }
            },
            {
              "name": "generation",
              "optional": true,
              "schema": {
                "kind": "optional",
                "of": {
                  "bits": 64,
                  "kind": "integer",
                  "maximum": 9007199254740991,
                  "minimum": 0,
                  "signed": false
                }
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "pointer",
        "payload": {
          "fields": [
            {
              "name": "phase",
              "optional": false,
              "schema": {
                "kind": "ref",
                "name": "PointerPhase"
              }
            },
            {
              "name": "slot",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            },
            {
              "name": "generation",
              "optional": false,
              "schema": {
                "bits": 64,
                "kind": "integer",
                "maximum": 9007199254740991,
                "minimum": 0,
                "signed": false
              }
            },
            {
              "name": "x",
              "optional": false,
              "schema": {
                "bits": 64,
                "kind": "float"
              }
            },
            {
              "name": "y",
              "optional": false,
              "schema": {
                "bits": 64,
                "kind": "float"
              }
            },
            {
              "name": "button",
              "optional": true,
              "schema": {
                "kind": "optional",
                "of": {
                  "bits": 32,
                  "kind": "integer",
                  "maximum": 4294967295,
                  "minimum": 0,
                  "signed": false
                }
              }
            },
            {
              "name": "modifiers",
              "optional": false,
              "schema": {
                "kind": "array",
                "of": {
                  "kind": "string"
                }
              }
            },
            {
              "name": "delta_x",
              "optional": true,
              "schema": {
                "kind": "optional",
                "of": {
                  "bits": 64,
                  "kind": "float"
                }
              }
            },
            {
              "name": "delta_y",
              "optional": true,
              "schema": {
                "kind": "optional",
                "of": {
                  "bits": 64,
                  "kind": "float"
                }
              }
            }
          ],
          "kind": "struct"
        }
      }
    ]
  },
  "WorkspaceEventBatch": {
    "fields": [
      {
        "name": "events",
        "optional": false,
        "schema": {
          "kind": "array",
          "of": {
            "kind": "ref",
            "name": "WorkspaceEvent"
          }
        }
      },
      {
        "name": "dropped",
        "optional": false,
        "schema": {
          "bits": 64,
          "kind": "integer",
          "maximum": 9007199254740991,
          "minimum": 0,
          "signed": false
        }
      }
    ],
    "kind": "struct",
    "serde": {}
  },
  "WorkspaceInfo": {
    "fields": [
      {
        "name": "name",
        "optional": false,
        "schema": {
          "kind": "string"
        }
      },
      {
        "name": "architecture",
        "optional": false,
        "schema": {
          "kind": "string"
        }
      },
      {
        "name": "image",
        "optional": false,
        "schema": {
          "kind": "string"
        }
      }
    ],
    "kind": "struct",
    "serde": {}
  },
  "WorkspaceLifecycleAction": {
    "kind": "enum",
    "serde": {
      "rename_all": "snake_case"
    },
    "variants": [
      {
        "name": "create",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "update",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "remove",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "start",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "stop",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "restart",
        "payload": {
          "kind": "unit"
        }
      }
    ]
  },
  "WorkspaceLifecycleChange": {
    "fields": [
      {
        "name": "workspace",
        "optional": false,
        "schema": {
          "kind": "string"
        }
      },
      {
        "name": "action",
        "optional": false,
        "schema": {
          "kind": "ref",
          "name": "WorkspaceLifecycleAction"
        }
      },
      {
        "name": "revision",
        "optional": false,
        "schema": {
          "bits": 64,
          "kind": "integer",
          "maximum": 9007199254740991,
          "minimum": 0,
          "signed": false
        }
      },
      {
        "name": "coalesced",
        "optional": false,
        "schema": {
          "bits": 64,
          "kind": "integer",
          "maximum": 9007199254740991,
          "minimum": 0,
          "signed": false
        }
      }
    ],
    "kind": "struct",
    "serde": {}
  },
  "WorkspaceMount": {
    "fields": [
      {
        "name": "host",
        "optional": false,
        "schema": {
          "kind": "string"
        }
      },
      {
        "name": "container",
        "optional": false,
        "schema": {
          "kind": "string"
        }
      },
      {
        "name": "read_only",
        "optional": false,
        "schema": {
          "kind": "boolean"
        }
      }
    ],
    "kind": "struct",
    "serde": {}
  },
  "WorkspaceState": {
    "fields": [
      {
        "name": "name",
        "optional": false,
        "schema": {
          "kind": "string"
        }
      },
      {
        "name": "architecture",
        "optional": false,
        "schema": {
          "kind": "string"
        }
      },
      {
        "name": "image",
        "optional": false,
        "schema": {
          "kind": "string"
        }
      },
      {
        "name": "running",
        "optional": false,
        "schema": {
          "kind": "boolean"
        }
      },
      {
        "name": "current",
        "optional": false,
        "schema": {
          "kind": "boolean"
        }
      }
    ],
    "kind": "struct",
    "serde": {}
  },
  "WorkspaceTerminal": {
    "fields": [
      {
        "name": "font_family",
        "optional": true,
        "schema": {
          "kind": "optional",
          "of": {
            "kind": "string"
          }
        }
      },
      {
        "name": "font_size",
        "optional": true,
        "schema": {
          "kind": "optional",
          "of": {
            "bits": 16,
            "kind": "integer",
            "maximum": 65535,
            "minimum": 0,
            "signed": false
          }
        }
      },
      {
        "name": "foreground",
        "optional": true,
        "schema": {
          "kind": "optional",
          "of": {
            "kind": "string"
          }
        }
      },
      {
        "name": "background",
        "optional": true,
        "schema": {
          "kind": "optional",
          "of": {
            "kind": "string"
          }
        }
      },
      {
        "name": "cursor_shape",
        "optional": true,
        "schema": {
          "kind": "optional",
          "of": {
            "kind": "string"
          }
        }
      },
      {
        "name": "cursor_blink",
        "optional": true,
        "schema": {
          "kind": "optional",
          "of": {
            "kind": "boolean"
          }
        }
      }
    ],
    "kind": "struct",
    "serde": {}
  }
};
const roots = {
  "failure": {
    "kind": "enum",
    "serde": {
      "rename_all": "snake_case",
      "tag": "error"
    },
    "variants": [
      {
        "name": "denied",
        "payload": {
          "fields": [
            {
              "name": "capability",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            },
            {
              "name": "detail",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "absent",
        "payload": {
          "fields": [
            {
              "name": "detail",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "conflict",
        "payload": {
          "fields": [
            {
              "name": "detail",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "failed",
        "payload": {
          "fields": [
            {
              "name": "detail",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "unsupported",
        "payload": {
          "fields": [
            {
              "name": "call",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            }
          ],
          "kind": "struct"
        }
      }
    ]
  },
  "reply": {
    "kind": "enum",
    "serde": {
      "content": "with",
      "rename_all": "snake_case",
      "tag": "reply"
    },
    "variants": [
      {
        "name": "workspace",
        "payload": {
          "kind": "newtype",
          "of": {
            "kind": "ref",
            "name": "WorkspaceInfo"
          }
        }
      },
      {
        "name": "workspace_configuration",
        "payload": {
          "kind": "newtype",
          "of": {
            "kind": "ref",
            "name": "WorkspaceConfiguration"
          }
        }
      },
      {
        "name": "workspaces",
        "payload": {
          "kind": "newtype",
          "of": {
            "kind": "array",
            "of": {
              "kind": "ref",
              "name": "WorkspaceState"
            }
          }
        }
      },
      {
        "name": "extensions",
        "payload": {
          "kind": "newtype",
          "of": {
            "kind": "array",
            "of": {
              "kind": "ref",
              "name": "ExtensionSummary"
            }
          }
        }
      },
      {
        "name": "extension",
        "payload": {
          "kind": "newtype",
          "of": {
            "kind": "ref",
            "name": "ExtensionSummary"
          }
        }
      },
      {
        "name": "extension_acquisition_job",
        "payload": {
          "kind": "newtype",
          "of": {
            "kind": "ref",
            "name": "ExtensionAcquisitionJob"
          }
        }
      },
      {
        "name": "extension_acquisition",
        "payload": {
          "kind": "newtype",
          "of": {
            "kind": "ref",
            "name": "ExtensionAcquisitionStatus"
          }
        }
      },
      {
        "name": "containers",
        "payload": {
          "kind": "newtype",
          "of": {
            "kind": "array",
            "of": {
              "kind": "ref",
              "name": "ContainerSummary"
            }
          }
        }
      },
      {
        "name": "container",
        "payload": {
          "kind": "newtype",
          "of": {
            "kind": "ref",
            "name": "ContainerSummary"
          }
        }
      },
      {
        "name": "processes",
        "payload": {
          "kind": "newtype",
          "of": {
            "kind": "ref",
            "name": "ProcessList"
          }
        }
      },
      {
        "name": "logs",
        "payload": {
          "kind": "newtype",
          "of": {
            "kind": "ref",
            "name": "ContainerOutput"
          }
        }
      },
      {
        "name": "execution",
        "payload": {
          "kind": "newtype",
          "of": {
            "kind": "ref",
            "name": "ExecutionSummary"
          }
        }
      },
      {
        "name": "executions",
        "payload": {
          "kind": "newtype",
          "of": {
            "kind": "ref",
            "name": "ExecutionList"
          }
        }
      },
      {
        "name": "images",
        "payload": {
          "kind": "newtype",
          "of": {
            "kind": "array",
            "of": {
              "kind": "ref",
              "name": "ImageSummary"
            }
          }
        }
      },
      {
        "name": "image",
        "payload": {
          "kind": "newtype",
          "of": {
            "kind": "ref",
            "name": "ImageSummary"
          }
        }
      },
      {
        "name": "image_pull_job",
        "payload": {
          "kind": "newtype",
          "of": {
            "kind": "ref",
            "name": "ImagePullJob"
          }
        }
      },
      {
        "name": "image_pull",
        "payload": {
          "kind": "newtype",
          "of": {
            "kind": "ref",
            "name": "ImagePullStatus"
          }
        }
      },
      {
        "name": "image_details",
        "payload": {
          "kind": "newtype",
          "of": {
            "kind": "ref",
            "name": "ImageDetails"
          }
        }
      },
      {
        "name": "image_prune",
        "payload": {
          "kind": "newtype",
          "of": {
            "kind": "ref",
            "name": "ImagePruneResult"
          }
        }
      },
      {
        "name": "volumes",
        "payload": {
          "kind": "newtype",
          "of": {
            "kind": "array",
            "of": {
              "kind": "ref",
              "name": "VolumeSummary"
            }
          }
        }
      },
      {
        "name": "volume",
        "payload": {
          "kind": "newtype",
          "of": {
            "kind": "ref",
            "name": "VolumeSummary"
          }
        }
      },
      {
        "name": "networks",
        "payload": {
          "kind": "newtype",
          "of": {
            "kind": "array",
            "of": {
              "kind": "ref",
              "name": "NetworkSummary"
            }
          }
        }
      },
      {
        "name": "network",
        "payload": {
          "kind": "newtype",
          "of": {
            "kind": "ref",
            "name": "NetworkSummary"
          }
        }
      },
      {
        "name": "tabs",
        "payload": {
          "kind": "newtype",
          "of": {
            "kind": "array",
            "of": {
              "kind": "ref",
              "name": "TabSummary"
            }
          }
        }
      },
      {
        "name": "topology",
        "payload": {
          "kind": "newtype",
          "of": {
            "kind": "ref",
            "name": "TerminalTopology"
          }
        }
      },
      {
        "name": "panes",
        "payload": {
          "kind": "newtype",
          "of": {
            "kind": "ref",
            "name": "PaneInventory"
          }
        }
      },
      {
        "name": "text",
        "payload": {
          "kind": "newtype",
          "of": {
            "kind": "ref",
            "name": "PaneText"
          }
        }
      },
      {
        "name": "semantics",
        "payload": {
          "kind": "newtype",
          "of": {
            "kind": "ref",
            "name": "PaneSemanticTree"
          }
        }
      },
      {
        "name": "entries",
        "payload": {
          "kind": "newtype",
          "of": {
            "kind": "array",
            "of": {
              "kind": "ref",
              "name": "Entry"
            }
          }
        }
      },
      {
        "name": "entry",
        "payload": {
          "kind": "newtype",
          "of": {
            "kind": "ref",
            "name": "Entry"
          }
        }
      },
      {
        "name": "contents",
        "payload": {
          "kind": "newtype",
          "of": {
            "kind": "array",
            "of": {
              "bits": 8,
              "kind": "integer",
              "maximum": 255,
              "minimum": 0,
              "signed": false
            }
          }
        }
      },
      {
        "name": "file_range",
        "payload": {
          "kind": "newtype",
          "of": {
            "kind": "ref",
            "name": "FileRange"
          }
        }
      },
      {
        "name": "identity",
        "payload": {
          "kind": "newtype",
          "of": {
            "kind": "string"
          }
        }
      },
      {
        "name": "done",
        "payload": {
          "kind": "unit"
        }
      }
    ]
  },
  "request": {
    "kind": "enum",
    "serde": {
      "content": "with",
      "deny_unknown_fields": true,
      "rename_all": "snake_case",
      "tag": "call"
    },
    "variants": [
      {
        "name": "workspace_info",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "workspace_list",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "workspace_inspect",
        "payload": {
          "fields": [
            {
              "name": "name",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "workspace_create",
        "payload": {
          "fields": [
            {
              "name": "configuration",
              "optional": false,
              "schema": {
                "kind": "ref",
                "name": "WorkspaceConfiguration"
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "workspace_adopt",
        "payload": {
          "fields": [
            {
              "name": "configuration",
              "optional": false,
              "schema": {
                "kind": "ref",
                "name": "WorkspaceConfiguration"
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "workspace_update",
        "payload": {
          "fields": [
            {
              "name": "name",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            },
            {
              "name": "generation",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            },
            {
              "name": "configuration",
              "optional": false,
              "schema": {
                "kind": "ref",
                "name": "WorkspaceConfiguration"
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "workspace_delete",
        "payload": {
          "fields": [
            {
              "name": "name",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            },
            {
              "name": "generation",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "workspace_start",
        "payload": {
          "fields": [
            {
              "name": "name",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "workspace_stop",
        "payload": {
          "fields": [
            {
              "name": "name",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "workspace_restart",
        "payload": {
          "fields": [
            {
              "name": "name",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "extension_list",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "extension_inspect",
        "payload": {
          "fields": [
            {
              "name": "name",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "extension_enable",
        "payload": {
          "fields": [
            {
              "name": "name",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            },
            {
              "name": "image_digest",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "extension_disable",
        "payload": {
          "fields": [
            {
              "name": "name",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            },
            {
              "name": "image_digest",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "extension_retry",
        "payload": {
          "fields": [
            {
              "name": "name",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            },
            {
              "name": "image_digest",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "extension_remove",
        "payload": {
          "fields": [
            {
              "name": "name",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            },
            {
              "name": "image_digest",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "extension_acquisition_start",
        "payload": {
          "fields": [
            {
              "name": "reference",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "extension_acquisition_status",
        "payload": {
          "fields": [
            {
              "name": "job",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "extension_acquisition_cancel",
        "payload": {
          "fields": [
            {
              "name": "job",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            },
            {
              "name": "revision",
              "optional": false,
              "schema": {
                "bits": 64,
                "kind": "integer",
                "maximum": 9007199254740991,
                "minimum": 0,
                "signed": false
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "extension_install",
        "payload": {
          "fields": [
            {
              "name": "job",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            },
            {
              "name": "revision",
              "optional": false,
              "schema": {
                "bits": 64,
                "kind": "integer",
                "maximum": 9007199254740991,
                "minimum": 0,
                "signed": false
              }
            },
            {
              "name": "granted",
              "optional": false,
              "schema": {
                "kind": "ref",
                "name": "Grant"
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "extension_update",
        "payload": {
          "fields": [
            {
              "name": "job",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            },
            {
              "name": "revision",
              "optional": false,
              "schema": {
                "bits": 64,
                "kind": "integer",
                "maximum": 9007199254740991,
                "minimum": 0,
                "signed": false
              }
            },
            {
              "name": "granted",
              "optional": false,
              "schema": {
                "kind": "ref",
                "name": "Grant"
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "container_list",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "container_inspect",
        "payload": {
          "fields": [
            {
              "name": "id",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "container_processes",
        "payload": {
          "fields": [
            {
              "name": "id",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "container_logs",
        "payload": {
          "fields": [
            {
              "name": "id",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            },
            {
              "name": "stdout",
              "optional": false,
              "schema": {
                "kind": "boolean"
              }
            },
            {
              "name": "stderr",
              "optional": false,
              "schema": {
                "kind": "boolean"
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "execution_inspect",
        "payload": {
          "fields": [
            {
              "name": "id",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "execution_list",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "execution_logs",
        "payload": {
          "fields": [
            {
              "name": "id",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            },
            {
              "name": "stdout",
              "optional": false,
              "schema": {
                "kind": "boolean"
              }
            },
            {
              "name": "stderr",
              "optional": false,
              "schema": {
                "kind": "boolean"
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "execution_wait",
        "payload": {
          "fields": [
            {
              "name": "id",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            },
            {
              "name": "timeout_ms",
              "optional": false,
              "schema": {
                "bits": 32,
                "kind": "integer",
                "maximum": 4294967295,
                "minimum": 0,
                "signed": false
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "execution_kill",
        "payload": {
          "fields": [
            {
              "name": "id",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            },
            {
              "name": "signal",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "execution_remove",
        "payload": {
          "fields": [
            {
              "name": "id",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "container_create",
        "payload": {
          "fields": [
            {
              "name": "spec",
              "optional": false,
              "schema": {
                "kind": "ref",
                "name": "ContainerCreateSpec"
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "container_start",
        "payload": {
          "fields": [
            {
              "name": "id",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "container_stop",
        "payload": {
          "fields": [
            {
              "name": "id",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "container_remove",
        "payload": {
          "fields": [
            {
              "name": "id",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "container_pause",
        "payload": {
          "fields": [
            {
              "name": "id",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "container_unpause",
        "payload": {
          "fields": [
            {
              "name": "id",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "container_restart",
        "payload": {
          "fields": [
            {
              "name": "id",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "container_rename",
        "payload": {
          "fields": [
            {
              "name": "id",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            },
            {
              "name": "name",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "container_kill",
        "payload": {
          "fields": [
            {
              "name": "id",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            },
            {
              "name": "signal",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "container_exec",
        "payload": {
          "fields": [
            {
              "name": "id",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            },
            {
              "name": "command",
              "optional": false,
              "schema": {
                "kind": "array",
                "of": {
                  "kind": "string"
                }
              }
            },
            {
              "name": "user",
              "optional": true,
              "schema": {
                "kind": "optional",
                "of": {
                  "kind": "string"
                }
              }
            },
            {
              "name": "working_directory",
              "optional": true,
              "schema": {
                "kind": "optional",
                "of": {
                  "kind": "string"
                }
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "container_attach_terminal",
        "payload": {
          "fields": [
            {
              "name": "id",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            },
            {
              "name": "command",
              "optional": false,
              "schema": {
                "kind": "array",
                "of": {
                  "kind": "string"
                }
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "image_list",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "image_pull",
        "payload": {
          "fields": [
            {
              "name": "reference",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "image_pull_start",
        "payload": {
          "fields": [
            {
              "name": "reference",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "image_pull_status",
        "payload": {
          "fields": [
            {
              "name": "job",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "image_pull_cancel",
        "payload": {
          "fields": [
            {
              "name": "job",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "image_inspect",
        "payload": {
          "fields": [
            {
              "name": "reference",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "image_remove",
        "payload": {
          "fields": [
            {
              "name": "reference",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "image_prune",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "volume_list",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "volume_inspect",
        "payload": {
          "fields": [
            {
              "name": "name",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "volume_create",
        "payload": {
          "fields": [
            {
              "name": "name",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "volume_remove",
        "payload": {
          "fields": [
            {
              "name": "name",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            },
            {
              "name": "generation",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "network_list",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "network_inspect",
        "payload": {
          "fields": [
            {
              "name": "reference",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "network_create",
        "payload": {
          "fields": [
            {
              "name": "name",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "network_remove",
        "payload": {
          "fields": [
            {
              "name": "reference",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "network_connect",
        "payload": {
          "fields": [
            {
              "name": "reference",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            },
            {
              "name": "container",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            },
            {
              "name": "aliases",
              "optional": true,
              "schema": {
                "kind": "array",
                "of": {
                  "kind": "string"
                }
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "network_disconnect",
        "payload": {
          "fields": [
            {
              "name": "reference",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            },
            {
              "name": "container",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "terminal_tabs",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "terminal_topology",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "pane_list",
        "payload": {
          "kind": "unit"
        }
      },
      {
        "name": "terminal_open_tab",
        "payload": {
          "fields": [
            {
              "name": "title",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "terminal_split",
        "payload": {
          "fields": [
            {
              "name": "slot",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            },
            {
              "name": "division",
              "optional": false,
              "schema": {
                "kind": "ref",
                "name": "Division"
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "terminal_split_observed",
        "payload": {
          "fields": [
            {
              "name": "slot",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            },
            {
              "name": "generation",
              "optional": false,
              "schema": {
                "bits": 64,
                "kind": "integer",
                "maximum": 9007199254740991,
                "minimum": 0,
                "signed": false
              }
            },
            {
              "name": "revision",
              "optional": false,
              "schema": {
                "bits": 64,
                "kind": "integer",
                "maximum": 9007199254740991,
                "minimum": 0,
                "signed": false
              }
            },
            {
              "name": "division",
              "optional": false,
              "schema": {
                "kind": "ref",
                "name": "Division"
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "terminal_spawn",
        "payload": {
          "fields": [
            {
              "name": "slot",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            },
            {
              "name": "command",
              "optional": false,
              "schema": {
                "kind": "array",
                "of": {
                  "kind": "string"
                }
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "terminal_spawn_observed",
        "payload": {
          "fields": [
            {
              "name": "slot",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            },
            {
              "name": "generation",
              "optional": false,
              "schema": {
                "bits": 64,
                "kind": "integer",
                "maximum": 9007199254740991,
                "minimum": 0,
                "signed": false
              }
            },
            {
              "name": "revision",
              "optional": false,
              "schema": {
                "bits": 64,
                "kind": "integer",
                "maximum": 9007199254740991,
                "minimum": 0,
                "signed": false
              }
            },
            {
              "name": "command",
              "optional": false,
              "schema": {
                "kind": "array",
                "of": {
                  "kind": "string"
                }
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "terminal_read_pane",
        "payload": {
          "fields": [
            {
              "name": "slot",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            },
            {
              "name": "lines",
              "optional": true,
              "schema": {
                "kind": "optional",
                "of": {
                  "bits": 64,
                  "kind": "integer",
                  "maximum": 9007199254740991,
                  "minimum": 0,
                  "signed": false
                }
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "pane_semantic_read",
        "payload": {
          "fields": [
            {
              "name": "slot",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "pane_semantic_action",
        "payload": {
          "fields": [
            {
              "name": "slot",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            },
            {
              "name": "action",
              "optional": false,
              "schema": {
                "kind": "ref",
                "name": "PaneSemanticAction"
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "terminal_write_pane",
        "payload": {
          "fields": [
            {
              "name": "slot",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            },
            {
              "name": "generation",
              "optional": false,
              "schema": {
                "bits": 64,
                "kind": "integer",
                "maximum": 9007199254740991,
                "minimum": 0,
                "signed": false
              }
            },
            {
              "name": "revision",
              "optional": false,
              "schema": {
                "bits": 64,
                "kind": "integer",
                "maximum": 9007199254740991,
                "minimum": 0,
                "signed": false
              }
            },
            {
              "name": "contents",
              "optional": false,
              "schema": {
                "kind": "array",
                "of": {
                  "bits": 8,
                  "kind": "integer",
                  "maximum": 255,
                  "minimum": 0,
                  "signed": false
                }
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "terminal_resize_grid",
        "payload": {
          "fields": [
            {
              "name": "slot",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            },
            {
              "name": "columns",
              "optional": false,
              "schema": {
                "bits": 16,
                "kind": "integer",
                "maximum": 65535,
                "minimum": 0,
                "signed": false
              }
            },
            {
              "name": "rows",
              "optional": false,
              "schema": {
                "bits": 16,
                "kind": "integer",
                "maximum": 65535,
                "minimum": 0,
                "signed": false
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "terminal_resize_grid_observed",
        "payload": {
          "fields": [
            {
              "name": "slot",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            },
            {
              "name": "generation",
              "optional": false,
              "schema": {
                "bits": 64,
                "kind": "integer",
                "maximum": 9007199254740991,
                "minimum": 0,
                "signed": false
              }
            },
            {
              "name": "revision",
              "optional": false,
              "schema": {
                "bits": 64,
                "kind": "integer",
                "maximum": 9007199254740991,
                "minimum": 0,
                "signed": false
              }
            },
            {
              "name": "columns",
              "optional": false,
              "schema": {
                "bits": 16,
                "kind": "integer",
                "maximum": 65535,
                "minimum": 0,
                "signed": false
              }
            },
            {
              "name": "rows",
              "optional": false,
              "schema": {
                "bits": 16,
                "kind": "integer",
                "maximum": 65535,
                "minimum": 0,
                "signed": false
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "terminal_close_pane",
        "payload": {
          "fields": [
            {
              "name": "slot",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "terminal_close_pane_observed",
        "payload": {
          "fields": [
            {
              "name": "slot",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            },
            {
              "name": "generation",
              "optional": false,
              "schema": {
                "bits": 64,
                "kind": "integer",
                "maximum": 9007199254740991,
                "minimum": 0,
                "signed": false
              }
            },
            {
              "name": "revision",
              "optional": false,
              "schema": {
                "bits": 64,
                "kind": "integer",
                "maximum": 9007199254740991,
                "minimum": 0,
                "signed": false
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "terminal_focus_pane",
        "payload": {
          "fields": [
            {
              "name": "slot",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "terminal_focus_pane_observed",
        "payload": {
          "fields": [
            {
              "name": "slot",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            },
            {
              "name": "generation",
              "optional": false,
              "schema": {
                "bits": 64,
                "kind": "integer",
                "maximum": 9007199254740991,
                "minimum": 0,
                "signed": false
              }
            },
            {
              "name": "revision",
              "optional": false,
              "schema": {
                "bits": 64,
                "kind": "integer",
                "maximum": 9007199254740991,
                "minimum": 0,
                "signed": false
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "terminal_retitle_pane",
        "payload": {
          "fields": [
            {
              "name": "slot",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            },
            {
              "name": "title",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "terminal_retitle_pane_observed",
        "payload": {
          "fields": [
            {
              "name": "slot",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            },
            {
              "name": "generation",
              "optional": false,
              "schema": {
                "bits": 64,
                "kind": "integer",
                "maximum": 9007199254740991,
                "minimum": 0,
                "signed": false
              }
            },
            {
              "name": "revision",
              "optional": false,
              "schema": {
                "bits": 64,
                "kind": "integer",
                "maximum": 9007199254740991,
                "minimum": 0,
                "signed": false
              }
            },
            {
              "name": "title",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "terminal_ratio",
        "payload": {
          "fields": [
            {
              "name": "slot",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            },
            {
              "name": "ratio",
              "optional": false,
              "schema": {
                "bits": 64,
                "kind": "float"
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "terminal_ratio_observed",
        "payload": {
          "fields": [
            {
              "name": "slot",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            },
            {
              "name": "generation",
              "optional": false,
              "schema": {
                "bits": 64,
                "kind": "integer",
                "maximum": 9007199254740991,
                "minimum": 0,
                "signed": false
              }
            },
            {
              "name": "revision",
              "optional": false,
              "schema": {
                "bits": 64,
                "kind": "integer",
                "maximum": 9007199254740991,
                "minimum": 0,
                "signed": false
              }
            },
            {
              "name": "ratio",
              "optional": false,
              "schema": {
                "bits": 64,
                "kind": "float"
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "terminal_switch_occupant",
        "payload": {
          "fields": [
            {
              "name": "slot",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            },
            {
              "name": "generation",
              "optional": false,
              "schema": {
                "bits": 64,
                "kind": "integer",
                "maximum": 9007199254740991,
                "minimum": 0,
                "signed": false
              }
            },
            {
              "name": "target",
              "optional": false,
              "schema": {
                "kind": "ref",
                "name": "PaneOccupantTarget"
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "terminal_switch_occupant_observed",
        "payload": {
          "fields": [
            {
              "name": "slot",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            },
            {
              "name": "generation",
              "optional": false,
              "schema": {
                "bits": 64,
                "kind": "integer",
                "maximum": 9007199254740991,
                "minimum": 0,
                "signed": false
              }
            },
            {
              "name": "revision",
              "optional": false,
              "schema": {
                "bits": 64,
                "kind": "integer",
                "maximum": 9007199254740991,
                "minimum": 0,
                "signed": false
              }
            },
            {
              "name": "target",
              "optional": false,
              "schema": {
                "kind": "ref",
                "name": "PaneOccupantTarget"
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "filesystem_list",
        "payload": {
          "fields": [
            {
              "name": "path",
              "optional": false,
              "schema": {
                "kind": "ref",
                "name": "RelativePath"
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "filesystem_read",
        "payload": {
          "fields": [
            {
              "name": "path",
              "optional": false,
              "schema": {
                "kind": "ref",
                "name": "RelativePath"
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "filesystem_read_range",
        "payload": {
          "fields": [
            {
              "name": "path",
              "optional": false,
              "schema": {
                "kind": "ref",
                "name": "RelativePath"
              }
            },
            {
              "name": "offset",
              "optional": false,
              "schema": {
                "bits": 64,
                "kind": "integer",
                "maximum": 9007199254740991,
                "minimum": 0,
                "signed": false
              }
            },
            {
              "name": "limit",
              "optional": false,
              "schema": {
                "bits": 64,
                "kind": "integer",
                "maximum": 9007199254740991,
                "minimum": 0,
                "signed": false
              }
            },
            {
              "name": "observed",
              "optional": true,
              "schema": {
                "kind": "optional",
                "of": {
                  "kind": "string"
                }
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "filesystem_stat",
        "payload": {
          "fields": [
            {
              "name": "path",
              "optional": false,
              "schema": {
                "kind": "ref",
                "name": "RelativePath"
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "filesystem_write",
        "payload": {
          "fields": [
            {
              "name": "path",
              "optional": false,
              "schema": {
                "kind": "ref",
                "name": "RelativePath"
              }
            },
            {
              "name": "contents",
              "optional": false,
              "schema": {
                "kind": "array",
                "of": {
                  "bits": 8,
                  "kind": "integer",
                  "maximum": 255,
                  "minimum": 0,
                  "signed": false
                }
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "filesystem_create_observed",
        "payload": {
          "fields": [
            {
              "name": "path",
              "optional": false,
              "schema": {
                "kind": "ref",
                "name": "RelativePath"
              }
            },
            {
              "name": "contents",
              "optional": false,
              "schema": {
                "kind": "array",
                "of": {
                  "bits": 8,
                  "kind": "integer",
                  "maximum": 255,
                  "minimum": 0,
                  "signed": false
                }
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "filesystem_mkdir",
        "payload": {
          "fields": [
            {
              "name": "path",
              "optional": false,
              "schema": {
                "kind": "ref",
                "name": "RelativePath"
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "filesystem_rename",
        "payload": {
          "fields": [
            {
              "name": "from",
              "optional": false,
              "schema": {
                "kind": "ref",
                "name": "RelativePath"
              }
            },
            {
              "name": "to",
              "optional": false,
              "schema": {
                "kind": "ref",
                "name": "RelativePath"
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "filesystem_rename_observed",
        "payload": {
          "fields": [
            {
              "name": "from",
              "optional": false,
              "schema": {
                "kind": "ref",
                "name": "RelativePath"
              }
            },
            {
              "name": "to",
              "optional": false,
              "schema": {
                "kind": "ref",
                "name": "RelativePath"
              }
            },
            {
              "name": "observed",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "filesystem_remove",
        "payload": {
          "fields": [
            {
              "name": "path",
              "optional": false,
              "schema": {
                "kind": "ref",
                "name": "RelativePath"
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "filesystem_remove_observed",
        "payload": {
          "fields": [
            {
              "name": "path",
              "optional": false,
              "schema": {
                "kind": "ref",
                "name": "RelativePath"
              }
            },
            {
              "name": "observed",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "interface_open_tab",
        "payload": {
          "fields": [
            {
              "name": "title",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "interface_split",
        "payload": {
          "fields": [
            {
              "name": "slot",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            },
            {
              "name": "division",
              "optional": false,
              "schema": {
                "kind": "ref",
                "name": "Division"
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "interface_withdraw",
        "payload": {
          "fields": [
            {
              "name": "slot",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "interface_render",
        "payload": {
          "fields": [
            {
              "name": "frame",
              "optional": false,
              "schema": {
                "kind": "ref",
                "name": "Frame"
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "interface_render_at",
        "payload": {
          "fields": [
            {
              "name": "slot",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            },
            {
              "name": "frame",
              "optional": false,
              "schema": {
                "kind": "ref",
                "name": "Frame"
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "source_resize",
        "payload": {
          "fields": [
            {
              "name": "mutation",
              "optional": false,
              "schema": {
                "kind": "ref",
                "name": "SourceMutation"
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "source_resize_at",
        "payload": {
          "fields": [
            {
              "name": "slot",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            },
            {
              "name": "mutation",
              "optional": false,
              "schema": {
                "kind": "ref",
                "name": "SourceMutation"
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "event_subscribe",
        "payload": {
          "fields": [
            {
              "name": "topic",
              "optional": false,
              "schema": {
                "kind": "ref",
                "name": "Topic"
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "event_unsubscribe",
        "payload": {
          "fields": [
            {
              "name": "topic",
              "optional": false,
              "schema": {
                "kind": "ref",
                "name": "Topic"
              }
            }
          ],
          "kind": "struct"
        }
      }
    ]
  },
  "snapshot": {
    "kind": "enum",
    "serde": {
      "content": "of",
      "rename_all": "snake_case",
      "tag": "snapshot"
    },
    "variants": [
      {
        "name": "containers",
        "payload": {
          "kind": "newtype",
          "of": {
            "kind": "array",
            "of": {
              "kind": "ref",
              "name": "ContainerSummary"
            }
          }
        }
      },
      {
        "name": "container_inventory",
        "payload": {
          "kind": "newtype",
          "of": {
            "kind": "ref",
            "name": "ContainerInventory"
          }
        }
      },
      {
        "name": "executions",
        "payload": {
          "kind": "newtype",
          "of": {
            "kind": "ref",
            "name": "ExecutionList"
          }
        }
      },
      {
        "name": "images",
        "payload": {
          "kind": "newtype",
          "of": {
            "kind": "array",
            "of": {
              "kind": "ref",
              "name": "ImageSummary"
            }
          }
        }
      },
      {
        "name": "image_pulls",
        "payload": {
          "kind": "newtype",
          "of": {
            "kind": "ref",
            "name": "ImagePullChange"
          }
        }
      },
      {
        "name": "volumes",
        "payload": {
          "kind": "newtype",
          "of": {
            "kind": "array",
            "of": {
              "kind": "ref",
              "name": "VolumeSummary"
            }
          }
        }
      },
      {
        "name": "networks",
        "payload": {
          "kind": "newtype",
          "of": {
            "kind": "array",
            "of": {
              "kind": "ref",
              "name": "NetworkSummary"
            }
          }
        }
      },
      {
        "name": "terminal",
        "payload": {
          "kind": "newtype",
          "of": {
            "kind": "array",
            "of": {
              "kind": "ref",
              "name": "TabSummary"
            }
          }
        }
      },
      {
        "name": "pane_changes",
        "payload": {
          "kind": "newtype",
          "of": {
            "kind": "ref",
            "name": "PaneChange"
          }
        }
      },
      {
        "name": "extensions",
        "payload": {
          "kind": "newtype",
          "of": {
            "kind": "array",
            "of": {
              "kind": "ref",
              "name": "ExtensionSummary"
            }
          }
        }
      },
      {
        "name": "extension_acquisitions",
        "payload": {
          "kind": "newtype",
          "of": {
            "kind": "ref",
            "name": "ExtensionAcquisitionChange"
          }
        }
      },
      {
        "name": "workspace_lifecycle",
        "payload": {
          "kind": "newtype",
          "of": {
            "kind": "ref",
            "name": "WorkspaceLifecycleChange"
          }
        }
      },
      {
        "name": "workspace_events",
        "payload": {
          "kind": "newtype",
          "of": {
            "kind": "ref",
            "name": "WorkspaceEventBatch"
          }
        }
      }
    ]
  },
  "uievent": {
    "kind": "enum",
    "serde": {
      "deny_unknown_fields": true,
      "tag": "interaction"
    },
    "variants": [
      {
        "name": "invoke",
        "payload": {
          "fields": [
            {
              "name": "trigger",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            },
            {
              "name": "node",
              "optional": false,
              "schema": {
                "bits": 64,
                "kind": "integer",
                "maximum": 9007199254740991,
                "minimum": 0,
                "signed": false
              }
            },
            {
              "name": "id",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            },
            {
              "name": "slot",
              "optional": true,
              "schema": {
                "kind": "optional",
                "of": {
                  "kind": "string"
                }
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "submit",
        "payload": {
          "fields": [
            {
              "name": "trigger",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            },
            {
              "name": "node",
              "optional": false,
              "schema": {
                "bits": 64,
                "kind": "integer",
                "maximum": 9007199254740991,
                "minimum": 0,
                "signed": false
              }
            },
            {
              "name": "id",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            },
            {
              "name": "slot",
              "optional": true,
              "schema": {
                "kind": "optional",
                "of": {
                  "kind": "string"
                }
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "change",
        "payload": {
          "fields": [
            {
              "name": "trigger",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            },
            {
              "name": "node",
              "optional": false,
              "schema": {
                "bits": 64,
                "kind": "integer",
                "maximum": 9007199254740991,
                "minimum": 0,
                "signed": false
              }
            },
            {
              "name": "id",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            },
            {
              "name": "slot",
              "optional": true,
              "schema": {
                "kind": "optional",
                "of": {
                  "kind": "string"
                }
              }
            },
            {
              "name": "value",
              "optional": false,
              "schema": {
                "kind": "ref",
                "name": "PropValue"
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "select",
        "payload": {
          "fields": [
            {
              "name": "trigger",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            },
            {
              "name": "node",
              "optional": false,
              "schema": {
                "bits": 64,
                "kind": "integer",
                "maximum": 9007199254740991,
                "minimum": 0,
                "signed": false
              }
            },
            {
              "name": "id",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            },
            {
              "name": "slot",
              "optional": true,
              "schema": {
                "kind": "optional",
                "of": {
                  "kind": "string"
                }
              }
            },
            {
              "name": "rows",
              "optional": false,
              "schema": {
                "kind": "array",
                "of": {
                  "bits": 64,
                  "kind": "integer",
                  "maximum": 9007199254740991,
                  "minimum": 0,
                  "signed": false
                }
              }
            },
            {
              "name": "collection",
              "optional": true,
              "schema": {
                "kind": "optional",
                "of": {
                  "kind": "ref",
                  "name": "UiCollectionSelection"
                }
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "edit",
        "payload": {
          "fields": [
            {
              "name": "trigger",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            },
            {
              "name": "node",
              "optional": false,
              "schema": {
                "bits": 64,
                "kind": "integer",
                "maximum": 9007199254740991,
                "minimum": 0,
                "signed": false
              }
            },
            {
              "name": "id",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            },
            {
              "name": "slot",
              "optional": true,
              "schema": {
                "kind": "optional",
                "of": {
                  "kind": "string"
                }
              }
            },
            {
              "name": "source",
              "optional": false,
              "schema": {
                "bits": 64,
                "kind": "integer",
                "maximum": 9007199254740991,
                "minimum": 0,
                "signed": false
              }
            },
            {
              "name": "version",
              "optional": false,
              "schema": {
                "bits": 64,
                "kind": "integer",
                "maximum": 9007199254740991,
                "minimum": 0,
                "signed": false
              }
            },
            {
              "name": "row",
              "optional": false,
              "schema": {
                "kind": "ref",
                "name": "UiSelectedRow"
              }
            },
            {
              "name": "column",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            },
            {
              "name": "value",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "sort",
        "payload": {
          "fields": [
            {
              "name": "trigger",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            },
            {
              "name": "node",
              "optional": false,
              "schema": {
                "bits": 64,
                "kind": "integer",
                "maximum": 9007199254740991,
                "minimum": 0,
                "signed": false
              }
            },
            {
              "name": "id",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            },
            {
              "name": "slot",
              "optional": true,
              "schema": {
                "kind": "optional",
                "of": {
                  "kind": "string"
                }
              }
            },
            {
              "name": "source",
              "optional": false,
              "schema": {
                "bits": 64,
                "kind": "integer",
                "maximum": 9007199254740991,
                "minimum": 0,
                "signed": false
              }
            },
            {
              "name": "version",
              "optional": false,
              "schema": {
                "bits": 64,
                "kind": "integer",
                "maximum": 9007199254740991,
                "minimum": 0,
                "signed": false
              }
            },
            {
              "name": "column",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            },
            {
              "name": "descending",
              "optional": false,
              "schema": {
                "kind": "boolean"
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "scroll",
        "payload": {
          "fields": [
            {
              "name": "trigger",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            },
            {
              "name": "node",
              "optional": false,
              "schema": {
                "bits": 64,
                "kind": "integer",
                "maximum": 9007199254740991,
                "minimum": 0,
                "signed": false
              }
            },
            {
              "name": "id",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            },
            {
              "name": "slot",
              "optional": true,
              "schema": {
                "kind": "optional",
                "of": {
                  "kind": "string"
                }
              }
            },
            {
              "name": "dx",
              "optional": false,
              "schema": {
                "bits": 64,
                "kind": "float"
              }
            },
            {
              "name": "dy",
              "optional": false,
              "schema": {
                "bits": 64,
                "kind": "float"
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "close",
        "payload": {
          "fields": [
            {
              "name": "trigger",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            },
            {
              "name": "node",
              "optional": false,
              "schema": {
                "bits": 64,
                "kind": "integer",
                "maximum": 9007199254740991,
                "minimum": 0,
                "signed": false
              }
            },
            {
              "name": "id",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            },
            {
              "name": "slot",
              "optional": true,
              "schema": {
                "kind": "optional",
                "of": {
                  "kind": "string"
                }
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "context",
        "payload": {
          "fields": [
            {
              "name": "trigger",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            },
            {
              "name": "node",
              "optional": false,
              "schema": {
                "bits": 64,
                "kind": "integer",
                "maximum": 9007199254740991,
                "minimum": 0,
                "signed": false
              }
            },
            {
              "name": "id",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            },
            {
              "name": "slot",
              "optional": true,
              "schema": {
                "kind": "optional",
                "of": {
                  "kind": "string"
                }
              }
            },
            {
              "name": "x",
              "optional": false,
              "schema": {
                "bits": 64,
                "kind": "float"
              }
            },
            {
              "name": "y",
              "optional": false,
              "schema": {
                "bits": 64,
                "kind": "float"
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "key",
        "payload": {
          "fields": [
            {
              "name": "trigger",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            },
            {
              "name": "node",
              "optional": false,
              "schema": {
                "bits": 64,
                "kind": "integer",
                "maximum": 9007199254740991,
                "minimum": 0,
                "signed": false
              }
            },
            {
              "name": "id",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            },
            {
              "name": "slot",
              "optional": true,
              "schema": {
                "kind": "optional",
                "of": {
                  "kind": "string"
                }
              }
            },
            {
              "name": "key",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            },
            {
              "name": "keycode",
              "optional": false,
              "schema": {
                "bits": 32,
                "kind": "integer",
                "maximum": 4294967295,
                "minimum": 0,
                "signed": false
              }
            },
            {
              "name": "modifiers",
              "optional": false,
              "schema": {
                "bits": 32,
                "kind": "integer",
                "maximum": 4294967295,
                "minimum": 0,
                "signed": false
              }
            },
            {
              "name": "pressed",
              "optional": false,
              "schema": {
                "kind": "boolean"
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "focus",
        "payload": {
          "fields": [
            {
              "name": "trigger",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            },
            {
              "name": "node",
              "optional": false,
              "schema": {
                "bits": 64,
                "kind": "integer",
                "maximum": 9007199254740991,
                "minimum": 0,
                "signed": false
              }
            },
            {
              "name": "id",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            },
            {
              "name": "slot",
              "optional": true,
              "schema": {
                "kind": "optional",
                "of": {
                  "kind": "string"
                }
              }
            },
            {
              "name": "focused",
              "optional": false,
              "schema": {
                "kind": "boolean"
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "pointer",
        "payload": {
          "fields": [
            {
              "name": "trigger",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            },
            {
              "name": "node",
              "optional": false,
              "schema": {
                "bits": 64,
                "kind": "integer",
                "maximum": 9007199254740991,
                "minimum": 0,
                "signed": false
              }
            },
            {
              "name": "id",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            },
            {
              "name": "slot",
              "optional": true,
              "schema": {
                "kind": "optional",
                "of": {
                  "kind": "string"
                }
              }
            },
            {
              "name": "phase",
              "optional": false,
              "schema": {
                "kind": "ref",
                "name": "UiPointerPhase"
              }
            },
            {
              "name": "x",
              "optional": true,
              "schema": {
                "kind": "optional",
                "of": {
                  "bits": 64,
                  "kind": "float"
                }
              }
            },
            {
              "name": "y",
              "optional": true,
              "schema": {
                "kind": "optional",
                "of": {
                  "bits": 64,
                  "kind": "float"
                }
              }
            },
            {
              "name": "button",
              "optional": false,
              "schema": {
                "bits": 32,
                "kind": "integer",
                "maximum": 4294967295,
                "minimum": 0,
                "signed": false
              }
            },
            {
              "name": "modifiers",
              "optional": false,
              "schema": {
                "bits": 32,
                "kind": "integer",
                "maximum": 4294967295,
                "minimum": 0,
                "signed": false
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "drag",
        "payload": {
          "fields": [
            {
              "name": "trigger",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            },
            {
              "name": "node",
              "optional": false,
              "schema": {
                "bits": 64,
                "kind": "integer",
                "maximum": 9007199254740991,
                "minimum": 0,
                "signed": false
              }
            },
            {
              "name": "id",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            },
            {
              "name": "slot",
              "optional": true,
              "schema": {
                "kind": "optional",
                "of": {
                  "kind": "string"
                }
              }
            }
          ],
          "kind": "struct"
        }
      },
      {
        "name": "drop",
        "payload": {
          "fields": [
            {
              "name": "trigger",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            },
            {
              "name": "node",
              "optional": false,
              "schema": {
                "bits": 64,
                "kind": "integer",
                "maximum": 9007199254740991,
                "minimum": 0,
                "signed": false
              }
            },
            {
              "name": "id",
              "optional": false,
              "schema": {
                "kind": "string"
              }
            },
            {
              "name": "slot",
              "optional": true,
              "schema": {
                "kind": "optional",
                "of": {
                  "kind": "string"
                }
              }
            },
            {
              "name": "source",
              "optional": false,
              "schema": {
                "bits": 64,
                "kind": "integer",
                "maximum": 9007199254740991,
                "minimum": 0,
                "signed": false
              }
            },
            {
              "name": "x",
              "optional": false,
              "schema": {
                "bits": 64,
                "kind": "float"
              }
            },
            {
              "name": "y",
              "optional": false,
              "schema": {
                "bits": 64,
                "kind": "float"
              }
            }
          ],
          "kind": "struct"
        }
      }
    ]
  }
};

function fail(path, expected) { throw new TypeError(`${path} must be ${expected}`); }
function validate(schema, value, path) {
  switch (schema.kind) {
    case 'unit': if (value !== undefined && value !== null) fail(path, 'absent'); return;
    case 'string': if (typeof value !== 'string') fail(path, 'a string'); return;
    case 'boolean': if (typeof value !== 'boolean') fail(path, 'a boolean'); return;
    case 'integer': if (!Number.isSafeInteger(value) || value < schema.minimum || value > schema.maximum) fail(path, `an integer from ${schema.minimum} through ${schema.maximum}`); return;
    case 'float': if (typeof value !== 'number' || !Number.isFinite(value)) fail(path, 'a finite number'); return;
    case 'optional': if (value !== null && value !== undefined) validate(schema.of, value, path); return;
    case 'newtype': return validate(schema.of, value, path);
    case 'array': if (!Array.isArray(value)) fail(path, 'an array'); value.forEach((entry, index) => validate(schema.of, entry, `${path}[${index}]`)); return;
    case 'tuple': {
      const fields = schema.items ?? schema.fields?.map((field) => field.schema) ?? [];
      if (fields.length === 1) return validate(fields[0], value, path);
      if (!Array.isArray(value) || value.length !== fields.length) fail(path, `a ${fields.length}-item tuple`);
      fields.forEach((field, index) => validate(field, value[index], `${path}[${index}]`)); return;
    }
    case 'map': if (!value || typeof value !== 'object' || Array.isArray(value)) fail(path, 'an object map'); for (const [key, entry] of Object.entries(value)) { validate(schema.key, key, path); validate(schema.value, entry, `${path}.${key}`); } return;
    case 'ref': return validate(definitions[schema.name], value, path);
    case 'struct':
      if (!value || typeof value !== 'object' || Array.isArray(value)) fail(path, 'an object');
      for (const field of schema.fields) {
        if (!field.optional && !(field.name in value)) fail(`${path}.${field.name}`, 'present');
        if (field.name in value) validate(field.schema, value[field.name], `${path}.${field.name}`);
      }
      if (schema.serde?.deny_unknown_fields) for (const key of Object.keys(value)) if (!schema.fields.some((field) => field.name === key)) fail(`${path}.${key}`, 'a declared field');
      return;
    case 'enum': return validateEnum(schema, value, path);
    default: throw new TypeError(`unsupported protocol schema kind ${schema.kind} at ${path}`);
  }
}
function validateEnum(schema, value, path) {
  const tag = schema.serde?.tag;
  const content = schema.serde?.content;
  if (tag) {
    if (!value || typeof value !== 'object' || Array.isArray(value) || typeof value[tag] !== 'string') fail(path, `an object tagged by ${tag}`);
    const variant = schema.variants.find((entry) => entry.name === value[tag]);
    if (!variant) fail(`${path}.${tag}`, 'a known variant');
    if (content) return validate(variant.payload, value[content], `${path}.${content}`);
    if (variant.payload.kind === 'unit') return;
    const body = { ...value }; delete body[tag]; return validate(variant.payload, body, path);
  }
  if (typeof value === 'string') {
    if (!schema.variants.some((entry) => entry.name === value && entry.payload.kind === 'unit')) fail(path, 'a known unit variant');
    return;
  }
  if (!value || typeof value !== 'object' || Array.isArray(value) || Object.keys(value).length !== 1) fail(path, 'an externally tagged variant');
  const [name] = Object.keys(value); const variant = schema.variants.find((entry) => entry.name === name);
  if (!variant) fail(path, 'a known variant'); validate(variant.payload, value[name], `${path}.${name}`);
}
export function validateRequest(value) { validate(roots.request, value, 'request'); return value; }
export function validateReply(value) { validate(roots.reply, value, 'reply'); return value; }
export function validateReplyFor(call, value) {
  validateReply(value);
  const expected = PROTOCOL_REPLIES[call];
  if (expected === undefined) fail('call', 'a known operation');
  if (value.reply !== expected) fail('reply.reply', expected);
  return value;
}
export function validateFailure(value) { validate(roots.failure, value, 'failure'); return value; }
export function validateSnapshot(value) { validate(roots.snapshot, value, 'snapshot'); return value; }
export function validateUiEvent(value) { validate(roots.uievent, value, 'ui event'); return value; }
export function encodeRequest(call, payload) {
  return validateRequest(payload === undefined ? { call } : { call, with: payload });
}
