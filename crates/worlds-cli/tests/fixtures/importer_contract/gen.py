#!/usr/bin/env python3
"""Regenerate the importer-contract fixtures (see README.md).

Run from anywhere: `python3 gen.py`. It rewrites every case directory next
to this file from one base export (one 10x6 room, a small game's enums and
entity types) plus a per-case change. Standard library only.
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
    {"slug": "block_material", "values": ["stone", "iron", "wood"]},
    {"slug": "skin", "values": ["none", "greybox", "cave", "stone", "forest"]},
    {"slug": "background", "values": ["greybox", "black", "cave", "castle", "outside"]},
    {"slug": "music", "values": ["silence", "title", "cave", "boss"]},
    {"slug": "species", "values": ["green_slime", "blue_slime", "bat", "golem"]},
    {"slug": "exit_direction", "values": ["right", "left", "up", "down"]},
]
TYPES = [["boulder", "Boulder", ["object", "puzzle"]],
         ["burnable_barrier", "BurnableBarrier", ["object", "puzzle"]],
         ["enemy", "Enemy", ["enemy"]], ["entity", "Entity", []],
         ["floor_button", "FloorButton", ["object", "puzzle"]], ["gate", "Gate", ["object", "puzzle"]],
         ["grapple_anchor", "GrappleAnchor", ["object", "traversal"]],
         ["lever", "Lever", ["object", "puzzle"]], ["locked_door", "LockedDoor", ["object", "puzzle"]],
         ["pickup", "Pickup", ["object", "item"]], ["push_block", "PushBlock", ["object", "puzzle"]],
         ["room_entry", "RoomEntry", ["room"]], ["room_exit", "RoomExit", ["room"]],
         ["save_shrine", "SaveShrine", ["object", "spawn"]], ["torch", "Torch", ["object", "puzzle"]],
         ["whip_point", "WhipPoint", ["object", "traversal"]]]

G = 32
W, H = 10, 6
COLLISION = ["##########"] + ["#........#"] * (H - 2) + ["##########"]
LEGEND = {"#": "solid", "-": "oneway", ".": None, "/": "ramp_r", "L": "ramp_l", "b": "breakable",
          "f": "fortified", "x": "hazard", "~": "water"}


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


def skin_tiles():
    out = []
    for y, row in enumerate(COLLISION):
        for x, c in enumerate(row):
            if c == "#":
                out.append([x * G, y * G, (x + y) % 4, 0, 0])
    return out


def base():
    world = {
        "format": 1, "name": "fixture", "grid_size": G, "bg_color": "#696A79",
        "collision_layer": "collision",
        "blocking_values": ["solid", "breakable", "fortified", "ramp_r", "ramp_l"],
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
        "fields": {"ambient_color": "#0A080F", "background": "black", "darkness": None,
                   "display_name": "Fixture room", "music": "silence", "skin": "greybox"},
        "fields_set": ["background", "display_name", "music", "skin"],
        "int_grids": {"collision": {"name": "Collision", "grid_size": G, "legend": LEGEND,
                                    "rows": COLLISION}},
        # Bottom to top, in the order the importer draws them (DecorBack z -2,
        # Skin z -1, DecorFront z 2), so no layer-order warning.
        "tile_layers": [
            {"layer": "decor_back", "name": "DecorBack", "kind": "tiles", "grid_size": G,
             "tilesets": [run("tiles", [32, 32], [[64, 64, 1, 1, 0]])]},
            {"layer": "main_tiling", "name": "Main_Tiling", "kind": "auto", "grid_size": G,
             "tilesets": [run("tiles", [32, 32], skin_tiles())]},
            {"layer": "decor_front", "name": "DecorFront", "kind": "tiles", "grid_size": G,
             "tilesets": []},
        ],
        "entities": [{"id": "entryaaaaa", "type": "room_entry", "layer": "entities",
                      "cell": [2, 4], "px": [80, 160], "size": [1, 1], "pivot": [0.5, 1.0],
                      "fields": {"facing": "right", "id": "main"},
                      "fields_set": ["facing", "id"]}],
        "notes": [],
    }
    return world, enums, level


def layer(level, slug):
    return next(l for l in level["tile_layers"] if l["layer"] == slug)


def insert_before_skin(level, entry):
    i = next(i for i, l in enumerate(level["tile_layers"]) if l["layer"] == "main_tiling")
    level["tile_layers"].insert(i, entry)


def pillars(x):
    return {"layer": "pillars", "name": "Pillars", "kind": "auto", "grid_size": G,
            "tilesets": [run("tiles", [32, 32], [[x, 64, 2, 1, 0], [x, 96, 2, 1, 0],
                                                  [x, 128, 2, 1, 0]])]}


# ---- cases: name -> (change, expect) ----

def c_two_auto(w, e, l):
    insert_before_skin(l, pillars(96))


def c_offgrid_pillars(w, e, l):
    insert_before_skin(l, pillars(112))


def c_decor_16px(w, e, l):
    d = layer(l, "decor_back")
    d["grid_size"] = 16
    d["tilesets"] = [run("tiles16", [16, 16], [[48, 48, 2, 1, 0], [64, 48, 3, 1, 0],
                                                [80, 48, 4, 1, 0], [48, 64, 2, 2, 0]])]


def c_all_flips(w, e, l):
    # Flips 0..3 on row 1, 4..7 (transposed) on row 3.
    tiles = [[32 + 32 * (f % 4), 32 if f < 4 else 96, 1, 1, f] for f in range(8)]
    layer(l, "decor_back")["tilesets"] = [run("tiles", [32, 32], tiles)]


def c_transposed_nonsquare(w, e, l):
    layer(l, "decor_back")["tilesets"] = [run("tiles64x32", [64, 32], [[64, 64, 1, 0, 4]])]


def c_flip_out_of_range(w, e, l):
    layer(l, "decor_back")["tilesets"][0]["tiles"][0][4] = 8


def c_skin_offgrid(w, e, l):
    layer(l, "main_tiling")["tilesets"][0]["tiles"][1][0] += 16


def c_skin_16px(w, e, l):
    s = layer(l, "main_tiling")
    s["tilesets"].append(run("tiles16", [16, 16], [[96, 96, 0, 2, 0]]))


def c_skin_stacked(w, e, l):
    tiles = layer(l, "main_tiling")["tilesets"][0]["tiles"]
    tiles.append(list(tiles[0][:2]) + [3, 1, 0])


def c_unknown_tiles_layer(w, e, l):
    insert_before_skin(l, {"layer": "trim", "name": "Trim", "kind": "tiles", "grid_size": G,
                           "tilesets": [run("tiles", [32, 32], [[96, 96, 0, 1, 0]])]})


def c_reserved_layer_name(w, e, l):
    insert_before_skin(l, {"layer": "objects_art", "name": "Objects", "kind": "auto",
                           "grid_size": G,
                           "tilesets": [run("tiles", [32, 32], [[96, 96, 0, 1, 0]])]})


def c_empty_unknown_tiles_layer(w, e, l):
    insert_before_skin(l, {"layer": "trim", "name": "Trim", "kind": "tiles", "grid_size": G,
                           "tilesets": []})


def c_reserved_breakables(w, e, l):
    insert_before_skin(l, {"layer": "rubble", "name": "Breakables", "kind": "auto",
                           "grid_size": G,
                           "tilesets": [run("tiles", [32, 32], [[96, 96, 0, 1, 0]])]})


def c_empty_layer_name(w, e, l):
    layer(l, "decor_back")["name"] = ""


def c_layer_name_collision(w, e, l):
    # "Back.Art" and "Back:Art" are both the node "Back_Art".
    for slug, name in (("decor_back_a", "Back.Art"), ("decor_back_b", "Back:Art")):
        insert_before_skin(l, {"layer": slug, "name": name, "kind": "tiles", "grid_size": G,
                               "tilesets": [run("tiles", [32, 32], [[96, 96, 0, 1, 0]])]})


def c_layer_order(w, e, l):
    # decor_front listed (drawn) below main_tiling.
    front = layer(l, "decor_front")
    l["tile_layers"].remove(front)
    insert_before_skin(l, front)


def c_project_grid_16(w, e, l):
    w["grid_size"] = 16
    l["grid_size"] = 16
    l["size"] = [W * 2, H * 2]


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


CASES = {
    "clean_minimal": (None, True, ""),
    "two_auto_layers": (c_two_auto, True, ""),
    "offgrid_pillars": (c_offgrid_pillars, True, ""),
    "decor_16px": (c_decor_16px, True, ""),
    "all_flips": (c_all_flips, True, ""),
    "transposed_nonsquare": (c_transposed_nonsquare, False, "only square tiles can be transposed"),
    "flip_out_of_range": (c_flip_out_of_range, False, "outside 0..7"),
    "skin_offgrid": (c_skin_offgrid, False, "off the 32 px grid"),
    "skin_16px": (c_skin_16px, False, "not 32x32 px"),
    "skin_stacked": (c_skin_stacked, False, "share a cell"),
    "unknown_tiles_layer": (c_unknown_tiles_layer, False, "decor_back* or decor_front*"),
    "reserved_layer_name": (c_reserved_layer_name, False, "reserved name"),
    "empty_unknown_tiles_layer": (c_empty_unknown_tiles_layer, True, ""),
    "reserved_breakables": (c_reserved_breakables, False, "reserved name"),
    "empty_layer_name": (c_empty_layer_name, False, "empty node name"),
    "layer_name_collision": (c_layer_name_collision, False, "would both be the node"),
    "layer_order": (c_layer_order, True, ""),
    "project_grid_16": (c_project_grid_16, False, "project grid is 16 px"),
    "no_collision_layer": (c_no_collision_layer, False, "no collision_layer"),
    "derived_int_grid": (c_derived_int_grid, True, ""),
    "annotations": (c_annotations, True, ""),
    "dangling_note_link": (c_dangling_note_link, True, ""),
}
# A substring the warning carries (an importer running the same cases words
# its own warnings, so it is the part any wording shares).
WARNINGS = {"dangling_note_link": "nothere22",
            "layer_order": "Worlds draws main_tiling above decor_front"}


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
        if change:
            change(w, e, l)
        dump(d / "world.json", w)
        dump(d / "enums.json", e)
        dump(d / "levels" / "room.json", l)
        (d / "tiles.png").write_bytes(image)
        expect = {"accept": accept, "error_contains": error}
        if name in WARNINGS:
            expect["warning_contains"] = WARNINGS[name]
        dump(d / "expect.json", expect)


if __name__ == "__main__":
    main()
