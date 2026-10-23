#!/usr/bin/env python3
"""Regenerate the `game` test fixture: a small platformer project
(`game.worlds/`) and its tileset art (`assets/`).

Everything here is drawn by this script (procedural tiles, hand-placed
rooms), so the fixture is free to use. Needs Pillow. After running it,
write the schemas and save the project once with Worlds, so the files are
in canonical form (`fixture_game.rs` checks they are):

    python3 tests/fixtures/game/make.py
    cargo run -p worlds-cli -- schema tests/fixtures/game/game.worlds
    cargo test -p worlds-core --test fixture_game -- --ignored canonicalize
"""

import json
import random
import shutil
from pathlib import Path

from PIL import Image, ImageDraw

HERE = Path(__file__).resolve().parent
PROJECT = HERE / "game.worlds"
ASSETS = HERE / "assets"
GRID = 32


# ---------------------------------------------------------------- art


def tile_art(img, col, row, base, seed, style):
    """Draw one 32x32 tile at (col, row) in `img`."""
    rng = random.Random(seed)
    x0, y0 = col * GRID, row * GRID
    d = ImageDraw.Draw(img)
    r, g, b = base
    for y in range(GRID):
        for x in range(GRID):
            n = rng.randint(-18, 18)
            if style == "brick":
                mortar = y % 8 == 0 or (x + (8 if (y // 8) % 2 else 0)) % 16 == 0
                k = -40 if mortar else n
            elif style == "edge":
                k = 40 - y * 3 + n if y < 10 else n
            elif style == "water":
                k = int(12 * ((x + y * 2) % 7 == 0)) + n // 2
            else:
                k = n
            img.putpixel(
                (x0 + x, y0 + y),
                (
                    max(0, min(255, r + k)),
                    max(0, min(255, g + k)),
                    max(0, min(255, b + k)),
                    255 if style != "water" else 200,
                ),
            )
    d.rectangle([x0, y0, x0 + GRID - 1, y0 + GRID - 1], outline=(r // 3, g // 3, b // 3, 255))


def make_tiles():
    """`tiles.png`: 48 x 24 tiles. Rows 0-6 the terrain the rules use,
    rows 8-15 background bricks, cols 34-35 rows 18-21 a pillar."""
    cols, rows = 48, 24
    img = Image.new("RGBA", (cols * GRID, rows * GRID), (0, 0, 0, 0))
    palette = {
        0: ((150, 120, 90), "edge"),  # wall tops
        1: ((120, 100, 80), "plain"),  # wall sides
        2: ((95, 80, 70), "brick"),  # wall fill
        3: ((80, 70, 60), "plain"),  # wall bottoms
        4: ((60, 110, 200), "water"),  # water
        5: ((170, 150, 100), "edge"),  # one-way platforms
        6: ((190, 60, 60), "plain"),  # hazards and the rest
    }
    for row, (base, style) in palette.items():
        for col in range(8):
            shade = tuple(max(0, min(255, c + (col - 4) * 6)) for c in base)
            tile_art(img, col, row, shade, row * 100 + col, style)
    for row in range(8, 16):
        for col in range(16):
            base = (40 + row * 4, 44 + col * 3, 70 + (row + col) % 5 * 6)
            tile_art(img, col, row, base, 1000 + row * 100 + col, "brick")
    for row in range(18, 22):
        for col in (34, 35):
            tile_art(img, col, row, (200, 190, 160), 5000 + row * 10 + col, "brick")
    img.save(ASSETS / "tiles.png")


def make_props():
    """`props.png`: 8 x 8 tiles of small things."""
    img = Image.new("RGBA", (8 * GRID, 8 * GRID), (0, 0, 0, 0))
    for row in range(8):
        for col in range(8):
            base = (60 + col * 22, 200 - row * 18, 90 + (col * row) % 7 * 20)
            tile_art(img, col, row, base, 9000 + row * 8 + col, "plain")
    img.save(ASSETS / "props.png")


# ---------------------------------------------------------------- project


def write(rel, value):
    path = PROJECT / rel
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, indent=2) + "\n")


def grid(w, h, fill="."):
    return [[fill] * w for _ in range(h)]


def rect(g, x, y, w, h, v):
    for yy in range(y, y + h):
        for xx in range(x, x + w):
            g[yy][xx] = v


def border(g, t=1):
    h, w = len(g), len(g[0])
    rect(g, 0, 0, w, t, "#")
    rect(g, 0, h - t, w, t, "#")
    rect(g, 0, 0, t, h, "#")
    rect(g, w - t, 0, t, h, "#")


def rows(g):
    return ["".join(r) for r in g]


def ent(id_, ty, cell, **kw):
    e = {"id": id_, "type": ty, "cell": list(cell)}
    e.update(kw)
    return e


def level(slug, name, id_, world_px, size, collision, decor=None, entities=None, fields=None):
    d = f"worlds/main/levels/{slug}"
    meta = {
        "$schema": "../../../../schema/level.json",
        "format": 1,
        "id": id_,
        "slug": slug,
        "name": name,
        "world_px": list(world_px),
        "size": list(size),
        "depth": 0,
    }
    if fields:
        meta["fields"] = fields
    write(f"{d}/level.json", meta)
    write(
        f"{d}/collision.json",
        {"$schema": "../../../../schema/intgrid.json", "format": 1, "layer": "collision", "rows": rows(collision)},
    )
    if decor:
        write(
            f"{d}/decor_back.json",
            {"$schema": "../../../../schema/tiles.json", "format": 1, "layer": "decor_back", "tiles": decor},
        )
    if entities:
        write(
            f"{d}/entities.json",
            {
                "$schema": "../../../../schema/entities.json",
                "format": 1,
                "layers": [{"layer": "entities", "entities": entities}],
            },
        )


def background(x0, y0, w, h, seed):
    rng = random.Random(seed)
    out = []
    for y in range(y0, y0 + h):
        for x in range(x0, x0 + w):
            out.append({"cell": [x, y], "tileset": "tiles", "tile": [rng.randrange(16), 8 + rng.randrange(8)]})
    return out


def make_project():
    if PROJECT.exists():
        shutil.rmtree(PROJECT)
    write(
        "project.json",
        {
            "$schema": "schema/project.json",
            "format": 1,
            "name": "game",
            "grid_size": GRID,
            "seed": 20261023,
            "bg_color": "#2A2B36",
            "collision_layer": "collision",
            "level_fields": [
                {"slug": "display_name", "type": "string", "nullable": True, "display": {"canvas": True, "world": True}},
                {"slug": "music", "type": "enum", "enum": "music", "nullable": True, "display": {"canvas": True, "world": True}},
            ],
        },
    )
    write(
        "enums.json",
        {
            "$schema": "schema/enums.json",
            "format": 1,
            "enums": [
                {"slug": "facing", "name": "Facing", "values": [{"slug": "right"}, {"slug": "left"}]},
                {
                    "slug": "exit_direction",
                    "name": "ExitDirection",
                    "values": [{"slug": "right"}, {"slug": "left"}, {"slug": "up"}, {"slug": "down"}],
                },
                {"slug": "music", "name": "Music", "values": [{"slug": "cave"}, {"slug": "calm"}, {"slug": "chase"}]},
            ],
        },
    )
    write(
        "tilesets.json",
        {
            "$schema": "schema/tilesets.json",
            "format": 1,
            "tilesets": [
                {"slug": "tiles", "image": "../assets/tiles.png", "tile_size": [GRID, GRID]},
                {"slug": "props", "image": "../assets/props.png", "tile_size": [GRID, GRID]},
            ],
        },
    )
    write(
        "layers.json",
        {
            "$schema": "schema/layers.json",
            "format": 1,
            "layers": [
                {"slug": "decor_back", "name": "DecorBack", "kind": "tiles", "tileset": "tiles"},
                {
                    "slug": "collision",
                    "name": "Collision",
                    "kind": "int_grid",
                    "values": [
                        {"slug": "solid", "name": "solid", "color": "#46464F", "glyph": "#"},
                        {"slug": "oneway", "name": "oneway", "color": "#A89C74", "glyph": "-"},
                        {"slug": "breakable", "name": "breakable", "color": "#8A8070", "glyph": "b"},
                        {"slug": "fortified", "name": "fortified", "color": "#7C7CB0", "glyph": "f"},
                        {"slug": "water", "name": "water", "color": "#3F7FD0", "glyph": "~"},
                        {"slug": "hazard", "name": "hazard", "color": "#B04040", "glyph": "x"},
                        {"slug": "ramp_r", "name": "ramp_r", "color": "#5F8F5F", "glyph": "/"},
                        {"slug": "ramp_l", "name": "ramp_l", "color": "#8F5F8F", "glyph": "L"},
                    ],
                },
                {"slug": "decor_front", "name": "DecorFront", "kind": "tiles", "tileset": "tiles"},
                {"slug": "entities", "name": "Entities", "kind": "entities"},
                {"slug": "main_tiling", "name": "MainTiling", "kind": "auto", "source": "collision"},
            ],
        },
    )
    make_entities()
    make_rules()
    write(
        "worlds/main/world.json",
        {"$schema": "../../schema/world.json", "format": 1, "slug": "main", "name": "Main", "notes": []},
    )
    make_levels()


def edef(slug, name, color, fields, size=(1, 1), pivot=None, resizable=None, tags=None):
    d = {"$schema": "../schema/entity.json", "format": 1, "slug": slug, "name": name}
    if tags:
        d["tags"] = tags
    d["size"] = list(size)
    if resizable:
        d["resizable"] = list(resizable)
    if pivot:
        d["pivot"] = list(pivot)
    d["color"] = color
    d["fields"] = fields
    return d


def field(slug, ty, **kw):
    f = {"slug": slug, "type": ty}
    f.update(kw)
    f["display"] = {"canvas": True, "world": True}
    return f


def make_entities():
    channel = field("channel", "int", default=0, min=0, max=31)
    defs = [
        edef(
            "room_entry",
            "RoomEntry",
            "#B3E6FF",
            [field("id", "string", default="main"), field("facing", "enum", enum="facing", default="right")],
            pivot=(0.5, 1.0),
            tags=["room"],
        ),
        edef(
            "room_exit",
            "RoomExit",
            "#FFB347",
            [
                field("target", "entity_ref", nullable=True, allowed_types=["room_entry"], cross_level=True),
                field("direction", "enum", enum="exit_direction", default="right"),
                field("locked_by", "string", nullable=True),
            ],
            size=(1, 3),
            resizable=(True, True),
            tags=["room"],
        ),
        edef(
            "floor_button",
            "FloorButton",
            "#E6C84A",
            [channel, field("latching", "bool", default=False)],
            pivot=(0.5, 1.0),
            tags=["object", "puzzle"],
        ),
        edef(
            "locked_door",
            "LockedDoor",
            "#A0522D",
            [channel, field("needs_item", "string", default="small_key"), field("consumes", "bool", default=True)],
            size=(1, 3),
            resizable=(True, True),
            pivot=(0.5, 1.0),
            tags=["object", "puzzle"],
        ),
        edef("lever", "Lever", "#8080FF", [channel, field("on", "bool", default=False)], pivot=(0.5, 1.0), tags=["object", "puzzle"]),
        edef("gate", "Gate", "#909090", [channel], size=(1, 3), pivot=(0.5, 1.0), tags=["object", "puzzle"]),
        edef("pickup", "Pickup", "#60D060", [field("item", "string", default="coin")], pivot=(0.5, 1.0), tags=["object"]),
        edef("push_block", "PushBlock", "#B08060", [field("heavy", "bool", default=False)], tags=["object"]),
        edef("save_point", "SavePoint", "#F0F0A0", [field("label", "string", nullable=True)], pivot=(0.5, 1.0), tags=["object"]),
        edef(
            "torch",
            "Torch",
            "#FF7F27",
            [channel, field("lit", "bool", default=True)],
            pivot=(0.5, 0.5),
            tags=["object", "puzzle"],
        ),
    ]
    for d in defs:
        write(f"entities/{d['slug']}.json", d)


def rule(slug, rows_, tiles, **kw):
    r = {"slug": slug, "match": {"rows": rows_}, "out": {"tileset": "tiles", "tiles": tiles}}
    r.update(kw)
    return r


def make_rules():
    rules = [
        rule("wall_top", ["*.*", "*#*", "***"], [[c, 0] for c in range(4)], **{"break": True}),
        rule("wall_bottom", ["***", "*#*", "*.*"], [[c, 3] for c in range(4)], **{"break": True}),
        rule("wall_side", ["***", ".#*", "***"], [[c, 1] for c in range(4)], symmetry=["mirror_x"], **{"break": True}),
        rule("wall_fill", ["#"], [[c, 2] for c in range(8)], out_of_bounds="solid"),
        rule("oneway", ["-"], [[c, 5] for c in range(4)]),
        rule("water_surface", ["*.*", "*~*", "***"], [[c, 4] for c in range(2)], **{"break": True}),
        rule("water_body", ["~"], [[c, 4] for c in range(2, 6)]),
        rule("hazard", ["x"], [[0, 6], [1, 6]]),
        rule("breakable", ["b"], [[2, 6], [3, 6]]),
        rule("fortified", ["f"], [[4, 6], [5, 6]]),
        rule("ramp_right", ["/"], [[6, 6]]),
        rule("ramp_left", ["L"], [[7, 6]]),
    ]
    write(
        "rules/main_tiling.json",
        {
            "$schema": "../schema/rules.json",
            "format": 1,
            "layer": "main_tiling",
            "groups": [{"slug": "base_tile", "name": "Base tile", "rules": rules}],
        },
    )


def make_levels():
    # crevasse: one tall room, 64 x 28, floor three cells thick, a few
    # stalactites hanging from the ceiling, a background over all of it.
    g = grid(64, 28)
    border(g)
    rect(g, 0, 25, 64, 3, "#")
    for x, n in [(8, 3), (20, 2), (41, 4), (52, 2)]:
        rect(g, x, 1, 2, n, "#")
    rect(g, 1, 1, 3, 2, "f")
    level(
        "crevasse",
        "Crevasse",
        "crevasse22",
        (0, -1920),
        (64, 28),
        g,
        decor=background(1, 1, 62, 24, 1),
        entities=[
            ent("entrymain2", "room_entry", (3, 24), fields={"id": "main"}),
            ent("torchaaaa2", "torch", (12, 18)),
            ent("pickupaaa2", "pickup", (40, 24)),
        ],
        fields={"display_name": "The Crevasse", "music": "cave"},
    )
    # level_1: a room with ledges and a pool.
    g = grid(19, 13)
    border(g)
    rect(g, 1, 11, 6, 1, "~")
    rect(g, 7, 8, 5, 1, "-")
    rect(g, 13, 5, 5, 1, "#")
    rect(g, 15, 10, 1, 1, "/")
    level(
        "level_1",
        "Level 1",
        "levelone22",
        (2048, -1280),
        (19, 13),
        g,
        decor=background(1, 1, 17, 10, 2),
        entities=[ent("leveraaaa2", "lever", (9, 7)), ent("pickupbbb2", "pickup", (16, 11))],
    )
    # level_2: a corridor with a button, a door and some spikes.
    g = grid(30, 8)
    border(g)
    rect(g, 12, 6, 3, 1, "x")
    rect(g, 22, 1, 2, 3, "b")
    level(
        "level_2",
        "Level 2",
        "leveltwo22",
        (2656, -1120),
        (30, 8),
        g,
        entities=[
            ent("buttonaaa2", "floor_button", (5, 6)),
            ent("dooraaaaa2", "locked_door", (26, 6)),
        ],
    )
    # level_3: a small cell, no entities.
    g = grid(14, 7)
    border(g)
    rect(g, 5, 4, 1, 2, "L")
    level("level_3", "Level 3", "levelthre2", (-448, -1280), (14, 7), g)
    # level_4: a big hall (two-thick walls) and a 3 x 3 closet in its
    # top-right corner.
    g = grid(27, 20)
    border(g, 2)
    rect(g, 19, 2, 6, 6, "#")
    rect(g, 20, 3, 3, 3, ".")
    rect(g, 6, 12, 4, 1, "-")
    level(
        "level_4",
        "Level 4",
        "levelfour2",
        (3616, -1120),
        (27, 20),
        g,
        entities=[ent("savepoint2", "save_point", (4, 17)), ent("blockaaaa2", "push_block", (12, 17))],
    )


if __name__ == "__main__":
    ASSETS.mkdir(parents=True, exist_ok=True)
    make_tiles()
    make_props()
    make_project()
