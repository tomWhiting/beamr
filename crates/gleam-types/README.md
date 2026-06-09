# gleam-types

`gleam-types` extracts public Gleam function type annotations into the compact
sidecar format consumed by `beamr`'s JIT and AOT compilation paths.

The library exposes the sidecar data model, serializer, deserializer, and a
source extractor for annotated Gleam modules. The bundled `gleam-types` CLI scans
a Gleam project, matches source modules with compiled `.beam` files, and writes
`.gleam_types` sidecars next to the beam artifacts.
