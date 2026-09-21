"""Builds the application icons from Logo.png.

The source is a glowing mark on a near-black ground. Shipped as-is it would
be a black square in the taskbar and on the Start Menu, sitting badly against
any theme, so the ground is turned into transparency: alpha comes from how
bright a pixel is, which keeps the glow fading out naturally instead of
stopping at a hard edge.

Run after changing Logo.png:

    python install/make-icons.py
"""

import pathlib

from PIL import Image

ROOT = pathlib.Path(__file__).resolve().parent.parent
SOURCE = ROOT / "Logo.png"
ASSETS = ROOT / "desktop" / "assets"

# Windows picks whichever of these fits the place it is drawing. Without the
# small ones it downscales the large one badly and the mark turns to mush in
# the taskbar.
ICO_SIZES = [16, 24, 32, 48, 64, 128, 256]

# What the window icon is decoded from at startup. Large enough for a
# high-resolution taskbar, small enough that decoding it costs nothing.
WINDOW_ICON = 256

# Drawn inside the interface, where it sits on the panel colour rather than on
# the desktop.
MARK = 128

# Where the ground ends and the mark begins. Measured rather than guessed:
# sampling the border of the source, which carries no mark, its brightest
# channel reaches 63. Anything at or below that is ground.
FLOOR = 64
# Where the mark becomes fully solid. The gap between the two is the fade, and
# keeping it short gives a clear silhouette — which matters far more at 16
# pixels than a long soft glow, and a long fade turns to grey mush there.
SOLID = 120


def keyed(image: Image.Image) -> Image.Image:
    """Returns the image with its dark ground turned into transparency."""
    image = image.convert("RGB")
    source = image.tobytes()
    out = bytearray(len(source) // 3 * 4)

    span = SOLID - FLOOR
    for index in range(0, len(source), 3):
        r, g, b = source[index], source[index + 1], source[index + 2]
        # The brightest channel rather than a luminance weighting: a saturated
        # blue is visually solid, and a luminance formula would make it half
        # transparent.
        brightness = r if r > g else g
        if b > brightness:
            brightness = b

        if brightness <= FLOOR:
            alpha = 0
        elif brightness >= SOLID:
            alpha = 255
        else:
            alpha = (brightness - FLOOR) * 255 // span

        at = index // 3 * 4
        out[at] = r
        out[at + 1] = g
        out[at + 2] = b
        out[at + 3] = alpha

    return Image.frombytes("RGBA", image.size, bytes(out))


def trim(image: Image.Image) -> Image.Image:
    """Crops the transparent margin, so the mark fills its icon."""
    box = image.getbbox()
    if not box:
        return image

    # Kept square, centred, so nothing is distorted when it is resized.
    left, top, right, bottom = box
    width, height = right - left, bottom - top
    side = max(width, height)
    centre_x, centre_y = left + width // 2, top + height // 2

    half = side // 2
    return image.crop((centre_x - half, centre_y - half, centre_x + half, centre_y + half))


def main() -> None:
    if not SOURCE.exists():
        raise SystemExit(f"{SOURCE} not found")

    ASSETS.mkdir(parents=True, exist_ok=True)
    mark = trim(keyed(Image.open(SOURCE)))
    print(f"source {SOURCE.name}, trimmed to {mark.size}")

    # A little padding, so the glow is not clipped by the icon's edge.
    padded = Image.new("RGBA", (int(mark.width * 1.06),) * 2, (0, 0, 0, 0))
    offset = (padded.width - mark.width) // 2
    padded.paste(mark, (offset, offset), mark)

    ico = ASSETS / "rhevia.ico"
    padded.save(ico, format="ICO", sizes=[(s, s) for s in ICO_SIZES])
    print(f"  {ico.relative_to(ROOT)}  {', '.join(str(s) for s in ICO_SIZES)}")

    for name, size in [("icon-256.png", WINDOW_ICON), ("mark-128.png", MARK)]:
        path = ASSETS / name
        padded.resize((size, size), Image.LANCZOS).save(path, format="PNG")
        print(f"  {path.relative_to(ROOT)}  {size}x{size}")


if __name__ == "__main__":
    main()
