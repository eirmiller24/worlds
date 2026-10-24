"""Draw the Worlds icon as pixel art and write it at several sizes.

A 64x64 grid: four 30x30 tiles split by a 4 px black cross, no outer
border. Top-left is a chunky W (so the icon reads as "Worlds" in a
taskbar), bottom-right is empty (transparent); bottom-left is the Earth,
top-right the Moon, each filling its whole tile. Larger sizes are
nearest-neighbour scale-ups, so every art pixel stays a sharp square.

    python3 assets/icon/make_icon.py
"""

import math
import random
from pathlib import Path

from PIL import Image, ImageDraw

TILE = 30
LINE = 4
SIZE = 2 * TILE + LINE  # 64
OUT = Path(__file__).resolve().parent
SCALES = [1, 2, 4, 8, 16]  # 64 .. 1024 px

LAND = 0.48  # noise height above which a pixel is land
BLACK = (0, 0, 0, 255)
CLEAR = (0, 0, 0, 0)


def value_noise(seed, cells):
    """Smooth noise on [0, TILE)^2 from a `cells` x `cells` random lattice."""
    rng = random.Random(seed)
    grid = [[rng.random() for _ in range(cells + 1)] for _ in range(cells + 1)]

    def smooth(t):
        return t * t * (3 - 2 * t)

    def at(x, y):
        fx, fy = x / TILE * cells, y / TILE * cells
        ix, iy = int(fx), int(fy)
        tx, ty = smooth(fx - ix), smooth(fy - iy)
        a = grid[iy][ix] + (grid[iy][ix + 1] - grid[iy][ix]) * tx
        b = grid[iy + 1][ix] + (grid[iy + 1][ix + 1] - grid[iy + 1][ix]) * tx
        return a + (b - a) * ty

    return at


def light(x, y):
    """Lit from the top-left: 1 at that corner, 0 at the opposite one."""
    return 1 - (x + y) / (2 * (TILE - 1))


def earth(seed=7):
    land = [value_noise(seed, 4), value_noise(seed + 4, 8)]
    ocean = [(16, 42, 110), (24, 64, 150), (38, 98, 190), (70, 140, 220)]
    green = [(28, 92, 40), (46, 128, 52), (80, 166, 66), (130, 200, 90)]
    px = {}
    for y in range(TILE):
        for x in range(TILE):
            h = 0.7 * land[0](x, y) + 0.3 * land[1](x, y)
            shade = light(x, y)
            # Ordered dithering between the shade steps, for a pixel-art ramp.
            bayer = ([[0, 2], [3, 1]][y % 2][x % 2] / 4 - 0.375) * 0.5
            if h > LAND:
                # Higher ground and the lit side are lighter.
                step = (h - LAND) / 0.2 * 1.3 + shade * 1.5 + bayer
                px[x, y] = green[max(0, min(3, int(step)))]
            else:
                # Shallows along the coast are lighter.
                step = 0.3 + shade * 1.9 + (1.0 if h > LAND - 0.04 else 0) + bayer
                px[x, y] = ocean[max(0, min(3, int(step)))]
    return px


CRATERS = [  # centre x, centre y, radius
    (8, 9, 4.5),
    (21, 20, 5.0),
    (23, 6, 2.5),
    (7, 23, 2.8),
    (15, 14, 1.6),
    (27, 27, 1.6),
    (13, 26, 1.3),
    (27, 13, 1.3),
    (3, 15, 1.1),
]


def moon():
    maria = value_noise(5, 3)
    rough = value_noise(3, 10)
    greys = [(64, 64, 70), (96, 96, 102), (128, 128, 134), (158, 158, 162), (186, 186, 188), (212, 212, 212)]
    px = {}
    for y in range(TILE):
        for x in range(TILE):
            # Bright highlands, darker soft patches (the "seas"), a little grit.
            step = 2.6 + light(x, y) * 1.4 + (rough(x, y) - 0.5) * 1.0
            if maria(x, y) < 0.42:
                step -= 1.0
            for cx, cy, r in CRATERS:
                dx, dy = x + 0.5 - cx, y + 0.5 - cy
                d = math.hypot(dx, dy)
                # Along the light direction: -1 at the top-left edge, 1 at the bottom-right.
                along = (dx + dy) / (math.sqrt(2) * max(d, 0.01))
                if d < r - 1.0:
                    # Floor: one step darker, with the shadow cast by the
                    # top-left wall.
                    step = 1.0 if along < -0.2 and d > r * 0.35 else 1.8
                elif d < r:
                    # Inner wall: shadowed toward the light, lit away from it.
                    step = 0.6 if along < 0.3 else 4.3
                elif d < r + 0.9 and along < -0.3:
                    # Outer rim catching the light.
                    step = 5.2
            px[x, y] = greys[max(0, min(5, int(step)))]
    return px


def letter_w(img):
    """A chunky pixel W in the top-left tile: four strokes of one zigzag,
    pale on a black outline so it reads on light and dark taskbars."""
    zig = [(3, 3), (9, 27), (15, 13), (21, 27), (27, 3)]
    d = ImageDraw.Draw(img)
    d.line(zig, fill=BLACK, width=8, joint="curve")
    d.line(zig, fill=(236, 236, 240, 255), width=4, joint="curve")


def draw():
    img = Image.new("RGBA", (SIZE, SIZE), CLEAR)
    for i in range(SIZE):
        for j in range(TILE, TILE + LINE):
            img.putpixel((i, j), BLACK)
            img.putpixel((j, i), BLACK)
    letter_w(img)
    for (x, y), c in earth(29).items():
        img.putpixel((x, TILE + LINE + y), c + (255,))
    for (x, y), c in moon().items():
        img.putpixel((TILE + LINE + x, y), c + (255,))
    return img


def main():
    img = draw()
    for s in SCALES:
        n = SIZE * s
        img.resize((n, n), Image.NEAREST).save(OUT / f"icon-{n}.png")


if __name__ == "__main__":
    main()
