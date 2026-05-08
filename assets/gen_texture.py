# /// script
# requires-python = ">=3.11"
# dependencies = ["pillow"]
# ///
"""Generate a 256x256 placeholder texture for the Vulkan cube demo.

Usage: `uv run assets/gen_texture.py` (writes assets/texture.png).

Design: coarse checkerboard with a single red cell at (0,0) so UV orientation
(U right, V down) is visible at a glance, plus a gradient tint so tiling on
the floor is easy to see.
"""
from PIL import Image, ImageDraw

SIZE = 256
CELL = 32

img = Image.new("RGBA", (SIZE, SIZE))
draw = ImageDraw.Draw(img)

for cy in range(SIZE // CELL):
    for cx in range(SIZE // CELL):
        on = (cx + cy) % 2 == 0
        if cx == 0 and cy == 0:
            color = (200, 70, 70, 255)  # UV origin marker
        elif on:
            color = (230, 230, 235, 255)
        else:
            color = (60, 60, 70, 255)
        x0, y0 = cx * CELL, cy * CELL
        draw.rectangle([x0, y0, x0 + CELL - 1, y0 + CELL - 1], fill=color)

# Subtle diagonal gradient tint to make floor tiling visible.
tint = Image.new("RGBA", (SIZE, SIZE))
td = ImageDraw.Draw(tint)
for y in range(SIZE):
    for x in range(SIZE):
        a = int(40 * (x + y) / (2 * SIZE))
        td.point((x, y), fill=(255, 180, 80, a))
img = Image.alpha_composite(img, tint)

out = "assets/texture.png"
img.save(out)
print(f"wrote {out} ({SIZE}x{SIZE})")
