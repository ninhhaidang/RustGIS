# OSM PBF ring-loss fixtures

Made-up data, constructed offline with Python's standard library from the published
[OSM PBF schemas](https://github.com/openstreetmap/OSM-binary/tree/master/osmpbf).
No data or implementation from another project is included. Each file has an uncompressed
`OSMHeader` declaring `OsmSchema-V0.6`, then one `OSMData` block. Nodes, ways and the relation
have separate primitive groups. Coordinates use the default 100-nanodegree granularity and
zero offsets. There are no timestamps, user details or downloaded map features.

The clean file has untagged nodes 1–6 at longitude/latitude (0,0), (10,0), (0,10), (1,1),
(2,1), (1,2). Way 10 references [1,2,3,1]; way 20 references [4,5,6,4]. Relation 50 has
`type=multipolygon`, with way 10 as `outer` and way 20 as `inner`. The string table is
["", "type", "multipolygon", "outer", "inner"]. No other tags are present.

Each lossy file changes just one thing:

| File | Difference from `hole-clean.osm.pbf` |
|---|---|
| `hole-missing-member.osm.pbf` | Omit way 20, retaining the relation's reference to it. |
| `hole-missing-node.osm.pbf` | Omit node 6, retaining way 20's reference to it. |
| `hole-unclosed.osm.pbf` | Way 20 references [4,5,6], without its closing node. |
| `hole-outside.osm.pbf` | Move nodes 4–6 to (21,21), (22,21), (21,22). |

The native reader produces one polygon with two rings for the clean file and one ring for
each lossy file. Each lossy file reports its omitted member way, ring with missing nodes,
unclosed ring chain or inner ring without an outer host. Native tests check the retained
outer geometry and exact counted notices; the command corpus requires successful imports
and delivery of these notices. Other OSM loss kinds are not certified by these fixtures.

The `hole-clean-with-point` and `hole-missing-member-with-point` variants add node 7 at
(0,0), tagged `name=Marker`; their string tables append `name` and `Marker`. They produce
points and polygons layers. The file-wide report is on the points layer, so the command
regression selects only polygons and checks that the same notice arrives exactly once
through local files, uploads and desktop read-ahead. Together the two files are 474 bytes.

| File | SHA-256 |
|---|---|
| `hole-clean.osm.pbf` | `b5d5a9d0256b5c420005fd9fe08da6bb51d87a896ab5d8e65f2f32d906d86934` |
| `hole-missing-member.osm.pbf` | `773140490c9dbcbb1914db863143b22528700922e07bf58bab24272b7ef1536e` |
| `hole-missing-node.osm.pbf` | `9c1c5e4d483415809e47e1a5c5f1fb1d682e83c98c1351355c78e2fbadebef77` |
| `hole-unclosed.osm.pbf` | `057ac714164461be6ce76960563d7786a40436a57518d26561584bf3480c8cd3` |
| `hole-outside.osm.pbf` | `2b0a1150e2dce48cddcf7533c27a336f054d0f23fbdf26b773bb746c375754dc` |
| `hole-clean-with-point.osm.pbf` | `6c2f3ee7c3196ad5a1b6dd1a8ab62fa52225bf277a6ae0e9122501a25860a314` |
| `hole-missing-member-with-point.osm.pbf` | `d09f119445cd7c21fc39477d5fc40918f714debdb5968b3a0712799d0cb29e97` |
