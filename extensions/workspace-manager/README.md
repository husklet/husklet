# Workspace resources

Husklet's first-party workspace resource extension. It lists and controls
containers, inspects their processes and bounded logs, and lists, pulls,
inspects, removes, or prunes local images. Image removal and pruning require an
explicit confirmation and report in-progress and failure states. It also inventories, inspects, creates, and safely removes volumes and
networks, and connects or disconnects stopped containers from networks.

The `containers`, `processes`, `images`, `volumes`, and `networks` pane providers select the matching
view when the extension is placed in a terminal pane.
