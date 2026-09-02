# Workspace resources

Husklet's first-party workspace resource extension. It lists and controls
containers, inspects their processes and bounded logs, and lists, pulls,
inspects, removes, or prunes local images. Destructive container, image, volume,
and network operations require an explicit in-pane confirmation and report
in-progress and retryable failure states. Opening or cancelling a prompt never
calls the host; only its final confirmation is marked destructive for semantic
clients. It also inventories, inspects, creates, and safely removes volumes and
networks, and connects or disconnects stopped containers from networks.

The `containers`, `processes`, `images`, `volumes`, and `networks` pane providers select the matching
view when the extension is placed in a terminal pane.
