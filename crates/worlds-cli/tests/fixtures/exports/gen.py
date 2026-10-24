#!/usr/bin/env python3
"""Regenerate the export fixtures (see README.md).

Run from anywhere: `python3 gen.py`. It rewrites every case directory next
to this file from one base export (one 10x6 level, a few enums and entity
types) plus a per-case change. Standard library only.
"""

import copy
import json
import shutil
import struct
import zlib
from pathlib import Path

HERE = Path(__file__).resolve().parent

ENUMS = [
    {"slug": "facing", "values": ["right", "left"]},
    {"slug": "material", "values": ["stone", "wood", "metal"]},
    {"slug": "music", "values": ["silence", "calm", "tense"]},
]
TYPES = [["door", "Door", ["object"]], ["enemy", "Enemy", ["enemy"]],
         ["pickup", "Pickup", ["object", "item"]], ["spawn", "Spawn", ["marker"]]]

G = 32
W, H = 10, 6
COLLISION = ["##########"] + ["#........#"] * (H - 2) + ["##########"]
LEGEND = {"#": "solid", "-": "oneway", ".": None, "~": "water"}


def png(width, height):
    """An RGBA PNG: each 16x16 block a different opaque color, its top-left
    pixel white so flips and turns show."""
    rows = []
    for y in range(height):
        row = bytearray([0])
        for x in range(width):
            bx, by = x // 16, y // 16
            if x % 16 == 0 and y % 16 == 0:
                row += bytes([255, 255, 255, 255])
            else:
                row += bytes([(bx * 37) % 256, (by * 71 + 40) % 256, ((bx + by) * 53) % 256, 255])
        rows.append(bytes(row))
    raw = b"".join(rows)

    def chunk(tag, data):
        return (struct.pack(">I", len(data)) + tag + data
                + struct.pack(">I", zlib.crc32(tag + data) & 0xFFFFFFFF))

    return (b"\x89PNG\r\n\x1a\n"
            + chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 6, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress(raw, 9)) + chunk(b"IEND", b""))


def run(tileset, tile_size, tiles):
    return {"tileset": tileset, "image": "tiles.png", "tile_size": tile_size, "tiles": tiles}


def wall_tiles(rows=COLLISION):
    out = []
    for y, row in enumerate(rows):
        for x, c in enumerate(row):
            if c == "#":
                out.append([x * G, y * G, (x + y) % 4, 0, 0])
    return out


def base():
    world = {
        "format": 1, "name": "fixture", "grid_size": G, "bg_color": "#696A79",
        "collision_layer": "collision",
        "blocking_values": ["solid"],
        "files": {"enums": "enums.json"},
        "entity_types": [{"slug": s, "name": n, "tags": t} for s, n, t in TYPES],
        "worlds": [{"slug": "main", "notes": [], "levels": [{
            "slug": "room", "id": "roomaaaaaa", "name": "Room", "file": "levels/room.json",
            "rect": [0, 0, W * G, H * G], "depth": 0, "touching": [],
            "open_edges": {"left": [], "right": [], "top": [], "bottom": []},
        }]}],
    }
    enums = {"format": 1, "enums": [
        {"slug": e["slug"], "name": e["slug"], "values": [{"slug": v, "name": v} for v in e["values"]]}
        for e in ENUMS]}
    level = {
        "format": 1, "slug": "room", "id": "roomaaaaaa", "name": "Room", "world": "main",
        "world_px": [0, 0], "size": [W, H], "grid_size": G, "depth": 0, "bg_color": "#696A79",
        "fields": {"display_name": "Fixture room", "music": "calm", "tint": None},
        "fields_set": ["display_name", "music"],
        "int_grids": {"collision": {"name": "Collision", "grid_size": G, "legend": LEGEND,
                                    "rows": COLLISION}},
        # Bottom to top: a hand-painted layer, the autotiled walls, an empty
        # hand-painted layer in front.
        "tile_layers": [
            {"layer": "back", "name": "Back", "kind": "tiles", "grid_size": G,
             "tilesets": [run("tiles", [32, 32], [[64, 64, 1, 1, 0]])]},
            {"layer": "walls", "name": "Walls", "kind": "auto", "grid_size": G,
             "tilesets": [run("tiles", [32, 32], wall_tiles())]},
            {"layer": "front", "name": "Front", "kind": "tiles", "grid_size": G,
             "tilesets": []},
        ],
        "entities": [{"id": "spawnaaaaa", "type": "spawn", "layer": "entities",
                      "cell": [2, 4], "px": [80, 160], "size": [1, 1], "pivot": [0.5, 1.0],
                      "fields": {"facing": "right", "id": "main"},
                      "fields_set": ["facing", "id"]}],
        "notes": [],
    }
    return world, enums, level


def layer(level, slug):
    return next(l for l in level["tile_layers"] if l["layer"] == slug)


def insert_before_walls(level, entry):
    i = next(i for i, l in enumerate(level["tile_layers"]) if l["layer"] == "walls")
    level["tile_layers"].insert(i, entry)


def pillars(x):
    return {"layer": "pillars", "name": "Pillars", "kind": "auto", "grid_size": G,
            "tilesets": [run("tiles", [32, 32], [[x, 64, 2, 1, 0], [x, 96, 2, 1, 0],
                                                  [x, 128, 2, 1, 0]])]}


# ---- cases: name -> (change, expect) ----

def c_two_auto(w, e, l):
    insert_before_walls(l, pillars(96))


def c_offgrid_auto_tiles(w, e, l):
    insert_before_walls(l, pillars(112))


def c_tiles_16px(w, e, l):
    d = layer(l, "back")
    d["grid_size"] = 16
    d["tilesets"] = [run("tiles16", [16, 16], [[48, 48, 2, 1, 0], [64, 48, 3, 1, 0],
                                                [80, 48, 4, 1, 0], [48, 64, 2, 2, 0]])]


def c_all_flips(w, e, l):
    # Flips 0..3 on row 1, 4..7 (transposed) on row 3.
    tiles = [[32 + 32 * (f % 4), 32 if f < 4 else 96, 1, 1, f] for f in range(8)]
    layer(l, "back")["tilesets"] = [run("tiles", [32, 32], tiles)]


def c_transposed_nonsquare(w, e, l):
    layer(l, "back")["tilesets"] = [run("tiles64x32", [64, 32], [[64, 64, 1, 0, 4]])]


def c_flip_out_of_range(w, e, l):
    layer(l, "back")["tilesets"][0]["tiles"][0][4] = 8


def c_no_collision_layer(w, e, l):
    del w["collision_layer"]
    del w["blocking_values"]
    del w["worlds"][0]["levels"][0]["open_edges"]


def c_derived_int_grid(w, e, l):
    l["int_grids"]["pillar_marks"] = {
        "name": "Pillar marks", "grid_size": G, "derived": True,
        "legend": {".": None, "p": "pillar_base"},
        "rows": ["." * W] * (H - 2) + ["...p..p..."] + ["." * W]}


def c_annotations(w, e, l):
    w["worlds"][0]["notes"] = [
        {"id": "pinstatuea", "kind": "pin", "title": "Statue",
         "tags": ["landmark"], "links": ["level:room"], "px": [96, 64],
         "icon": "statue", "label": "Statue"},
        {"id": "pathwalkaa", "kind": "path", "title": "Walk",
         "tags": ["route"], "links": ["note:pinstatuea", "note:pinpoolaaa"],
         "points": [[96, 64], [200, 64], [200, 150]]},
        {"id": "rgcavesaaa", "kind": "region", "title": "Caves",
         "tags": ["area", "caves"], "rect": [-32, -32, 400, 300]},
    ]
    l["notes"] = [{"id": "pinpoolaaa", "kind": "pin", "title": "Pool",
                   "tags": ["landmark"], "px": [200, 150], "icon": "pool"}]
    l["tags"] = ["caves"]
    w["worlds"][0]["levels"][0]["tags"] = ["caves"]


def c_dangling_note_link(w, e, l):
    w["worlds"][0]["notes"] = [
        {"id": "pathwalkaa", "kind": "path", "title": "Walk", "tags": ["route"],
         "links": ["note:nothere22"], "points": [[96, 64], [200, 64]]},
    ]


# An L: the room's top-right 4x3 cells are cut out (void), its walls close
# around the inside corner, and a second level, `annex`, sits in the notch.
SHAPED = ["######....", "#....#....", "#....#....", "#....#####", "#.........", "##########"]
SHAPED_VOID = [[6, 0, 4, 3]]
SHAPED_OUTLINE = [[0, 0], [192, 0], [192, 96], [320, 96], [320, 192], [0, 192]]


def c_shaped_level(w, e, l):
    entry = w["worlds"][0]["levels"][0]
    entry["void"] = SHAPED_VOID
    entry["outline"] = SHAPED_OUTLINE
    # The notch's wall (x 192) and floor (y 96) touch the annex.
    entry["touching"] = [
        {"level": "annex", "edge": "right", "span": [0, 96], "at": 192},
        {"level": "annex", "edge": "top", "span": [192, 320], "at": 96},
    ]
    # Runs follow the outline: [first, end, at]; the gap in the right wall
    # at row 4 is open (right edge at x 10).
    entry["open_edges"] = {"left": [], "right": [[4, 5, 10]], "top": [], "bottom": []}
    l["void"] = SHAPED_VOID
    l["outline"] = SHAPED_OUTLINE
    l["int_grids"]["collision"]["rows"] = SHAPED
    layer(l, "walls")["tilesets"] = [run("tiles", [32, 32], wall_tiles(SHAPED))]
    annex_rows = ["####", "#..#", "####"]
    w["worlds"][0]["levels"].append({
        "slug": "annex", "id": "annexaaaaa", "name": "Annex", "file": "levels/annex.json",
        "rect": [192, 0, 4 * G, 3 * G], "depth": 0,
        "touching": [
            {"level": "room", "edge": "left", "span": [0, 96], "at": 192},
            {"level": "room", "edge": "bottom", "span": [192, 320], "at": 96},
        ],
        "open_edges": {"left": [], "right": [], "top": [], "bottom": []},
    })
    return {"levels/annex.json": {
        "format": 1, "slug": "annex", "id": "annexaaaaa", "name": "Annex", "world": "main",
        "world_px": [192, 0], "size": [4, 3], "grid_size": G, "depth": 0, "bg_color": "#696A79",
        "fields": {"display_name": "Annex", "music": "calm", "tint": None},
        "fields_set": ["display_name"],
        "int_grids": {"collision": {"name": "Collision", "grid_size": G, "legend": LEGEND,
                                    "rows": annex_rows}},
        "tile_layers": [
            {"layer": "back", "name": "Back", "kind": "tiles", "grid_size": G, "tilesets": []},
            {"layer": "walls", "name": "Walls", "kind": "auto", "grid_size": G,
             "tilesets": [run("tiles", [32, 32], wall_tiles(annex_rows))]},
            {"layer": "front", "name": "Front", "kind": "tiles", "grid_size": G, "tilesets": []},
        ],
        "entities": [], "notes": [],
    }}


def c_shaped_wrong_outline(w, e, l):
    extra = c_shaped_level(w, e, l)
    # The notch drawn one cell too shallow.
    bad = [[0, 0], [192, 0], [192, 64], [320, 64], [320, 192], [0, 192]]
    w["worlds"][0]["levels"][0]["outline"] = bad
    l["outline"] = bad
    return extra


def c_shaped_wall_in_void(w, e, l):
    extra = c_shaped_level(w, e, l)
    rows = list(SHAPED)
    rows[1] = "#....#..#."
    l["int_grids"]["collision"]["rows"] = rows
    return extra


CASES = {
    "clean_minimal": (None, True, ""),
    "two_auto_layers": (c_two_auto, True, ""),
    "offgrid_auto_tiles": (c_offgrid_auto_tiles, True, ""),
    "tiles_16px": (c_tiles_16px, True, ""),
    "all_flips": (c_all_flips, True, ""),
    "transposed_nonsquare": (c_transposed_nonsquare, True, ""),
    "flip_out_of_range": (c_flip_out_of_range, False, "outside 0..7"),
    "no_collision_layer": (c_no_collision_layer, True, ""),
    "derived_int_grid": (c_derived_int_grid, True, ""),
    "annotations": (c_annotations, True, ""),
    "dangling_note_link": (c_dangling_note_link, True, ""),
    "shaped_level": (c_shaped_level, True, ""),
    "shaped_wrong_outline": (c_shaped_wrong_outline, False, "outline"),
    "shaped_wall_in_void": (c_shaped_wall_in_void, False, "in the level's void"),
}
# A substring the warning carries (another reader running the same cases
# words its own warnings, so it is the part any wording shares).
WARNINGS = {"dangling_note_link": "nothere22"}


def dump(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, indent=2) + "\n")


def main():
    image = png(128, 64)
    for name, (change, accept, error) in CASES.items():
        d = HERE / name
        if d.exists():
            shutil.rmtree(d)
        w, e, l = (copy.deepcopy(x) for x in base())
        # A change may return more files (path -> JSON) for extra levels.
        extra = (change(w, e, l) if change else None) or {}
        dump(d / "world.json", w)
        dump(d / "enums.json", e)
        dump(d / "levels" / "room.json", l)
        for rel, value in extra.items():
            dump(d / rel, value)
        (d / "tiles.png").write_bytes(image)
        expect = {"accept": accept, "error_contains": error}
        if name in WARNINGS:
            expect["warning_contains"] = WARNINGS[name]
        dump(d / "expect.json", expect)


if __name__ == "__main__":
    main()
